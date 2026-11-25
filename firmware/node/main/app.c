/* Badges, operator commands, stock custody and peer supervision. */

#include "node.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cJSON.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "nvs.h"

#define BATCHES 8
#define BATCH_NAME 12
/* Three missed heartbeats. */
#define OFFLINE_MS 15000
#define DEPART_STABLE_S 300

enum { IN_FRIDGE, IN_CARRIER, IN_QUARANTINE, AT_RECEIVER, IN_DISCARD };
enum { STORED, IN_TRANSIT, PENDING, CHECKED_OUT };
enum { OK, DO_NOT_USE, DISCARDED };

static const char *const places[] = {"fridge", "carrier", "quarantine", "receiver", "discard"};
static const char *const phases[] = {"stored", "transit", "pending", "out"};
static const char *const statuses[] = {"ok", "dnu", "discarded"};

typedef struct {
    char name[BATCH_NAME];
    char uid[UID_LEN + 1];
    uint8_t place;
    uint8_t phase;
    uint8_t status;
} batch_t;

/* Kept in NVS as one blob. */
static struct {
    batch_t batch[BATCHES];
    uint32_t known_lock[NODES];
    uint32_t seen_boot[NODES];
    uint32_t seen_seq[NODES];
    uint32_t transit;
} stock;

static struct {
    int64_t heard_ms;
    bool online, valid, excursion;
    int backlog;
} peer[NODES];

typedef struct {
    node_t from;
    char kind[12];
    char type[12];
    uint32_t boot, seq;
    char *body;
} message_t;

/* Last batch tag presented. */
static char tag[UID_LEN + 1];
static int64_t tag_ms;

static QueueHandle_t inbox;
static SemaphoreHandle_t lock;

static void stock_save(void)
{
    nvs_handle_t h;
    if (nvs_open("node", NVS_READWRITE, &h) == ESP_OK) {
        nvs_set_blob(h, "stock", &stock, sizeof stock);
        nvs_commit(h);
        nvs_close(h);
    }
}

void app_message(node_t from, const char *kind, uint32_t boot, uint32_t seq, const char *type,
                 const char *body)
{
    message_t m = {.from = from, .boot = boot, .seq = seq, .body = strdup(body)};
    strlcpy(m.kind, kind, sizeof m.kind);
    strlcpy(m.type, type, sizeof m.type);
    if (m.body && xQueueSend(inbox, &m, 0) != pdTRUE) {
        free(m.body);
    }
}

int app_stock_json(char *out, int size)
{
    xSemaphoreTake(lock, portMAX_DELAY);
    int n = snprintf(out, size, ",\"online\":{\"fridge\":%d,\"carrier\":%d},\"batches\":[",
                     peer[FRIDGE].online, peer[CARRIER].online);
    bool first = true;
    for (int i = 0; i < BATCHES && n < size; i++) {
        const batch_t *b = &stock.batch[i];
        if (b->name[0]) {
            n += snprintf(out + n, size - n, "%s[\"%s\",\"%s\",\"%s\",\"%s\"]", first ? "" : ",",
                          b->name, places[b->place], phases[b->phase], statuses[b->status]);
            first = false;
        }
    }
    if (n < size) {
        n += snprintf(out + n, size - n, "]");
    }
    xSemaphoreGive(lock);
    return n < size ? n : size - 1;
}

static const char *text(const cJSON *object, const char *key)
{
    const char *s = cJSON_GetStringValue(cJSON_GetObjectItemCaseSensitive(object, key));
    return s ? s : "";
}

static int number(const cJSON *object, const char *key)
{
    const cJSON *n = cJSON_GetObjectItemCaseSensitive(object, key);
    return cJSON_IsNumber(n) ? n->valueint : 0;
}

/* Text that goes into a record without escaping. */
static bool plain(const char *s, size_t max)
{
    size_t n = strlen(s);
    for (size_t i = 0; i < n; i++) {
        if ((unsigned char)s[i] < 0x20 || s[i] == '"' || s[i] == '\\') {
            return false;
        }
    }
    return n < max;
}

/* The unit that watches a place, or NODES for none. */
static node_t watcher(uint8_t place)
{
    return place == IN_CARRIER ? CARRIER : place <= IN_QUARANTINE ? FRIDGE : NODES;
}

static void mark_unusable(node_t unit)
{
    for (int i = 0; i < BATCHES; i++) {
        batch_t *b = &stock.batch[i];
        if (b->name[0] && watcher(b->place) == unit && b->phase != CHECKED_OUT && b->status == OK) {
            b->status = DO_NOT_USE;
            journal_add(CRITICAL, "batch", "{\"batch\":\"%s\",\"status\":\"dnu\",\"place\":\"%s\"}",
                        b->name, places[b->place]);
        }
    }
}

/* Also covers excursions whose event was missed. */
static void follow_lock(node_t place, uint32_t value)
{
    if (value > stock.known_lock[place]) {
        stock.known_lock[place] = value;
        mark_unusable(place);
        stock_save();
    }
}

static void move_carried(uint8_t from, uint8_t to)
{
    bool changed = false;
    for (int i = 0; i < BATCHES; i++) {
        batch_t *b = &stock.batch[i];
        if (b->name[0] && b->place == IN_CARRIER && b->phase == from) {
            b->phase = to;
            changed = true;
            journal_add(CRITICAL, "batch", "{\"batch\":\"%s\",\"phase\":\"%s\"}", b->name, phases[to]);
        }
    }
    if (changed) {
        stock_save();
    }
}

static void follow_transit(uint32_t transit)
{
    if (transit == stock.transit) {
        return;
    }
    stock.transit = transit;
    stock_save();
    if (transit == TRANSIT_ACTIVE) {
        move_carried(STORED, IN_TRANSIT);
    } else if (transit == TRANSIT_ARRIVED) {
        move_carried(STORED, IN_TRANSIT);
        move_carried(IN_TRANSIT, PENDING);
    }
}

static void heard(node_t from)
{
    peer[from].heard_ms = now_ms();
    if (!peer[from].online) {
        peer[from].online = true;
        journal_add(CRITICAL, "peer", "{\"peer\":\"%s\",\"online\":1}", node_names[from]);
    }
}

static void watch_peers(void)
{
    for (node_t n = 0; n < INVENTORY; n++) {
        if (peer[n].online && now_ms() - peer[n].heard_ms > OFFLINE_MS) {
            peer[n].online = false;
            peer[n].valid = false;
            journal_add(CRITICAL, "peer", "{\"peer\":\"%s\",\"online\":0}", node_names[n]);
        }
    }
}

static void on_peer(const message_t *m, const cJSON *body)
{
    if (strcmp(m->kind, "heartbeat") == 0) {
        heard(m->from);
    } else if (strcmp(m->kind, "telemetry") == 0 && strcmp(m->type, "telemetry") == 0) {
        peer[m->from].valid = number(body, "valid");
        peer[m->from].excursion = number(body, "excursion");
        peer[m->from].backlog = number(body, "backlog");
        follow_lock(m->from, number(body, "lock"));
        if (m->from == CARRIER) {
            follow_transit(number(body, "transit"));
        }
    } else if (strcmp(m->kind, "event") == 0) {
        bool newer = m->boot > stock.seen_boot[m->from]
                     || (m->boot == stock.seen_boot[m->from] && m->seq > stock.seen_seq[m->from]);
        if (!newer) {
            return;
        }
        stock.seen_boot[m->from] = m->boot;
        stock.seen_seq[m->from] = m->seq;
        if (strcmp(m->type, "excursion") == 0) {
            peer[m->from].excursion = number(body, "on");
            follow_lock(m->from, number(body, "lock"));
        }
        stock_save();
    }
}

static batch_t *find(const char *name)
{
    for (int i = 0; i < BATCHES; i++) {
        if (name[0] && strcmp(stock.batch[i].name, name) == 0) {
            return &stock.batch[i];
        }
    }
    return NULL;
}

static batch_t *vacant(void)
{
    /* Forget a dispensed batch only when needed. */
    for (int reuse = 0; reuse < 2; reuse++) {
        for (int i = 0; i < BATCHES; i++) {
            batch_t *b = &stock.batch[i];
            if (!b->name[0] || (reuse && b->phase == CHECKED_OUT)) {
                return b;
            }
        }
    }
    return NULL;
}

/* Why a place cannot vouch for its contents, or NULL. */
static const char *doubt(uint8_t at)
{
    node_t place = watcher(at);
    if (place == NODES) {
        return NULL;
    }
    if (!peer[place].online) {
        return "storage offline";
    }
    if (!peer[place].valid) {
        return "sensor fault";
    }
    return peer[place].excursion ? "excursion active" : NULL;
}

/* Returns why the request is refused, or NULL once done. */
static const char *handle_stock(const char *cmd, const char *name, const cJSON *body)
{
    const char *note = text(body, "note");
    batch_t *b = find(name);
    if (strcmp(cmd, "checkin") == 0) {
        uint8_t place = strcmp(text(body, "to"), "carrier") == 0 ? IN_CARRIER : IN_FRIDGE;
        if (b && b->phase != CHECKED_OUT) {
            return "already stored";
        }
        if (!name[0] || strlen(name) >= BATCH_NAME) {
            return "bad name";
        }
        const char *why = doubt(place);
        if (why) {
            return why;
        }
        if (!tag[0] || (b && strcmp(b->uid, tag) != 0)) {
            return "scan batch";
        }
        for (int i = 0; i < BATCHES; i++) {
            if (&stock.batch[i] != b && stock.batch[i].name[0] && strcmp(stock.batch[i].uid, tag) == 0) {
                return "tag in use";
            }
        }
        /* A returned batch keeps its verdict. */
        uint8_t status = b ? b->status : OK;
        if (!b && !(b = vacant())) {
            return "stock full";
        }
        strlcpy(b->name, name, sizeof b->name);
        strlcpy(b->uid, tag, sizeof b->uid);
        b->place = place;
        b->phase = STORED;
        b->status = status;
        return NULL;
    }
    if (!b) {
        return "unknown batch";
    }
    if (strcmp(b->uid, tag) != 0) {
        return "scan batch";
    }
    if (strcmp(cmd, "quarantine") == 0) {
        if (b->status != DO_NOT_USE || b->place != IN_FRIDGE || b->phase != STORED) {
            return "not locked in fridge";
        }
        b->place = IN_QUARANTINE;
        return NULL;
    }
    if (strcmp(cmd, "release") == 0 || strcmp(cmd, "discard") == 0) {
        if (!note[0]) {
            return "basis required";
        }
        if (b->status != DO_NOT_USE) {
            return "not locked";
        }
        bool release = strcmp(cmd, "release") == 0;
        const char *why = release && b->phase != CHECKED_OUT ? doubt(b->place) : NULL;
        if (why) {
            return why;
        }
        b->status = release ? OK : DISCARDED;
        if (!release) {
            b->place = IN_DISCARD;
            b->phase = STORED;
        } else if (b->place == IN_QUARANTINE) {
            b->place = IN_FRIDGE;
        }
        return NULL;
    }
    if (b->status != OK) {
        return b->status == DO_NOT_USE ? "do not use" : "discarded";
    }
    if (strcmp(cmd, "accept") == 0) {
        if (b->phase != PENDING) {
            return "not pending";
        }
        const char *why = doubt(IN_CARRIER);
        if (why) {
            return why;
        }
        /* The whole transport must be on record before a decision. */
        if (peer[CARRIER].backlog > 0) {
            return "records incomplete";
        }
        b->place = AT_RECEIVER;
        b->phase = STORED;
        return NULL;
    }
    const char *why = b->phase == CHECKED_OUT ? NULL : doubt(b->place);
    if (why) {
        return why;
    }
    if (b->phase != STORED) {
        return phases[b->phase];
    }
    if (strcmp(cmd, "checkout") == 0) {
        b->phase = CHECKED_OUT;
        return NULL;
    }
    if (strcmp(cmd, "load") == 0) {
        if (b->place != IN_FRIDGE) {
            return "not in fridge";
        }
        why = doubt(IN_CARRIER);
        if (why) {
            return why;
        }
        if (stock.transit != TRANSIT_NONE) {
            return "carrier away";
        }
        b->place = IN_CARRIER;
        return NULL;
    }
    return "unknown command";
}

static const char *handle_carrier(const char *cmd, const state_t *s)
{
    if (strcmp(cmd, "receiver") == 0) {
        state_transit(s->transit, true);
        return NULL;
    }
    if (strcmp(cmd, "depart") == 0) {
        if (s->transit != TRANSIT_NONE) {
            return "already departed";
        }
        if (!s->receiver_ready) {
            return "receiver not confirmed";
        }
        if (!s->now.temp_valid || s->stable_s < DEPART_STABLE_S) {
            return "temperature not stable";
        }
        if (!s->now.battery_valid || s->now.battery < BATTERY_LOW) {
            return "battery";
        }
        if (s->safe_s < TRANSPORT_S) {
            return "safe time";
        }
        if (s->now.door_open) {
            return "lid open";
        }
        state_transit(TRANSIT_ACTIVE, false);
        return NULL;
    }
    if (strcmp(cmd, "arrive") == 0) {
        if (s->transit != TRANSIT_ACTIVE) {
            return "not in transit";
        }
        state_transit(TRANSIT_ARRIVED, false);
        return NULL;
    }
    if (strcmp(cmd, "return") == 0) {
        if (s->transit != TRANSIT_ARRIVED) {
            return "not at receiver";
        }
        state_transit(TRANSIT_NONE, false);
        return NULL;
    }
    return "unknown command";
}

static void on_command(const message_t *m, const cJSON *body)
{
    /* Delivery may repeat. The host numbers commands upward. */
    static uint32_t boot, seq;
    if (m->boot < boot || (m->boot == boot && m->seq <= seq)) {
        return;
    }
    boot = m->boot;
    seq = m->seq;
    const char *cmd = text(body, "cmd"), *name = text(body, "batch"), *note = text(body, "note");
    if (!plain(cmd, 16) || !plain(name, BATCH_NAME) || !plain(note, 48)) {
        return;
    }
    state_t s = state_get();
    const char *why = NULL;
    if (!s.person[0]) {
        why = "badge required";
    } else if (journal_full()) {
        why = "log full";
    } else if (self == INVENTORY) {
        /* Custody changes only after its record is in flash. */
        static typeof(stock) before;
        before = stock;
        if (now_ms() - tag_ms > BADGE_MS) {
            tag[0] = '\0';
        }
        why = handle_stock(cmd, name, body);
        if (!why) {
            if (journal_add(CRITICAL, "op",
                            "{\"cmd\":\"%s\",\"batch\":\"%s\",\"ok\":1,\"person\":\"%s\",\"note\":\"%s\"}",
                            cmd, name, s.person, note)) {
                stock_save();
            } else {
                stock = before;
            }
            return;
        }
    } else if (self == CARRIER) {
        why = handle_carrier(cmd, &s);
        if (!why) {
            journal_add(CRITICAL, "op",
                        "{\"cmd\":\"%s\",\"ok\":1,\"person\":\"%s\",\"temp_c\":%.2f,\"battery\":%.1f,"
                        "\"box\":\"BOX-1\",\"note\":\"%s\"}",
                        cmd, s.person, s.now.temp_c, s.now.battery * 100, note);
            return;
        }
    } else {
        why = "unknown command";
    }
    journal_add(CRITICAL, "op",
                "{\"cmd\":\"%s\",\"batch\":\"%s\",\"ok\":0,\"person\":\"%s\",\"why\":\"%s\"}", cmd, name,
                s.person, why);
}

static void application(void *arg)
{
    for (;;) {
        message_t m;
        if (xQueueReceive(inbox, &m, pdMS_TO_TICKS(500)) == pdTRUE) {
            cJSON *body = cJSON_Parse(m.body);
            xSemaphoreTake(lock, portMAX_DELAY);
            if (body && strcmp(m.kind, "command") == 0) {
                on_command(&m, body);
            } else if (body && self == INVENTORY) {
                on_peer(&m, body);
            }
            xSemaphoreGive(lock);
            cJSON_Delete(body);
            free(m.body);
        }
        if (self == INVENTORY) {
            xSemaphoreTake(lock, portMAX_DELAY);
            watch_peers();
            xSemaphoreGive(lock);
        }
    }
}

static void badges(void *arg)
{
    for (;;) {
        char uid[UID_LEN + 1];
        if (bus_tag(uid)) {
            /* Staff badges share one prefix. All other tags are batches. */
            bool staff = strncmp(uid, "04", 2) == 0;
            if (staff) {
                state_person(uid);
            } else {
                xSemaphoreTake(lock, portMAX_DELAY);
                strlcpy(tag, uid, sizeof tag);
                tag_ms = now_ms();
                xSemaphoreGive(lock);
            }
            journal_add(CRITICAL, "scan", "{\"uid\":\"%s\",\"staff\":%d}", uid, staff);
        } else {
            journal_add(CRITICAL, "scan", "{\"error\":\"bad frame\"}");
        }
    }
}

void app_start(void)
{
    inbox = xQueueCreate(16, sizeof(message_t));
    lock = xSemaphoreCreateMutex();
    nvs_handle_t h;
    size_t size = sizeof stock;
    if (nvs_open("node", NVS_READONLY, &h) == ESP_OK) {
        if (nvs_get_blob(h, "stock", &stock, &size) != ESP_OK || size != sizeof stock) {
            memset(&stock, 0, sizeof stock);
        }
        nvs_close(h);
    }
    xTaskCreate(application, "app", 6144, NULL, 5, NULL);
    xTaskCreate(badges, "badge", 4096, NULL, 6, NULL);
}
