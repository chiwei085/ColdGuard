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
#define NOTE_MAX 48
/* Three missed heartbeats. */
#define OFFLINE_MS 15000
#define DEPART_STABLE_S 300
/* Bump with any change to stock_t. Older blobs are refused, not reinterpreted. */
#define STOCK_VERSION 2

typedef enum { IN_FRIDGE, IN_CARRIER, IN_QUARANTINE, AT_RECEIVER, IN_DISCARD, PLACES } place_t;
typedef enum { STORED, IN_TRANSIT, PENDING, CHECKED_OUT, PHASES } phase_t;
typedef enum { OK, DO_NOT_USE, DISCARDED, STATUSES } status_t;

static const char *const places[PLACES] = {
    [IN_FRIDGE] = "fridge",     [IN_CARRIER] = "carrier", [IN_QUARANTINE] = "quarantine",
    [AT_RECEIVER] = "receiver", [IN_DISCARD] = "discard",
};
static const char *const phases[PHASES] = {
    [STORED] = "stored", [IN_TRANSIT] = "transit", [PENDING] = "pending", [CHECKED_OUT] = "out"};
static const char *const statuses[STATUSES] = {
    [OK] = "ok", [DO_NOT_USE] = "dnu", [DISCARDED] = "discarded"};

typedef struct {
    char name[BATCH_NAME];
    char uid[UID_LEN + 1];
    uint8_t place;
    uint8_t phase;
    uint8_t status;
} batch_t;

/* Kept in NVS as one blob, so a change is saved whole or not at all. */
typedef struct {
    uint32_t version;
    batch_t batch[BATCHES];
    uint32_t known_count[NODES];
    uint32_t seen_boot[NODES];
    uint32_t seen_seq[NODES];
    uint32_t transit;
} stock_t;

typedef enum {
    CMD_CHECKIN,
    CMD_QUARANTINE,
    CMD_RELEASE,
    CMD_DISCARD,
    CMD_ACCEPT,
    CMD_CHECKOUT,
    CMD_LOAD,
    CMD_RECEIVER,
    CMD_DEPART,
    CMD_ARRIVE,
    CMD_RETURN,
    CMD_UNKNOWN,
} cmd_t;

static const char *const commands[CMD_UNKNOWN] = {
    [CMD_CHECKIN] = "checkin", [CMD_QUARANTINE] = "quarantine", [CMD_RELEASE] = "release",
    [CMD_DISCARD] = "discard", [CMD_ACCEPT] = "accept",         [CMD_CHECKOUT] = "checkout",
    [CMD_LOAD] = "load",       [CMD_RECEIVER] = "receiver",     [CMD_DEPART] = "depart",
    [CMD_ARRIVE] = "arrive",   [CMD_RETURN] = "return",
};

/* An operator command after validation at the receive boundary. */
typedef struct {
    cmd_t cmd;
    char word[16];
    char name[BATCH_NAME];
    char note[NOTE_MAX];
    /* Too long or not plain text. Refused, since the operator meant something. */
    bool bad_note;
    place_t to;
} command_t;

typedef struct {
    node_t from;
    char kind[12];
    char type[12];
    uint32_t boot, seq;
    char *body;
} message_t;

/* Guarded by stock_mutex. Changes are made on a copy and adopted once saved. */
static stock_t stock;
static struct {
    int64_t heard_ms;
    bool online, valid, excursion;
    uint32_t backlog;
} peer[NODES];
/* Last batch tag presented. */
static char tag[UID_LEN + 1];
static int64_t tag_ms;

static SemaphoreHandle_t stock_mutex;
static QueueHandle_t inbox;

static bool stock_save(const stock_t *next)
{
    nvs_handle_t h;
    if (nvs_open("node", NVS_READWRITE, &h) != ESP_OK) {
        return false;
    }
    bool ok = nvs_set_blob(h, "stock", next, sizeof *next) == ESP_OK && nvs_commit(h) == ESP_OK;
    nvs_close(h);
    return ok;
}

/* Adopts the copy once it is in flash. Otherwise the change is lost and may be repeated. */
static bool stock_commit(const stock_t *next)
{
    if (memcmp(next, &stock, sizeof stock) == 0) {
        return true;
    }
    if (!stock_save(next)) {
        return false;
    }
    stock = *next;
    return true;
}

static bool stock_valid(const stock_t *st)
{
    if (st->version != STOCK_VERSION || st->transit > TRANSIT_ARRIVED) {
        return false;
    }
    for (int i = 0; i < BATCHES; i++) {
        const batch_t *b = &st->batch[i];
        if (memchr(b->name, 0, sizeof b->name) == NULL ||
            memchr(b->uid, 0, sizeof b->uid) == NULL || b->place >= PLACES || b->phase >= PHASES ||
            b->status >= STATUSES) {
            return false;
        }
    }
    return true;
}

static void stock_load(void)
{
    nvs_handle_t h;
    size_t size = sizeof stock;
    esp_err_t err = nvs_open("node", NVS_READONLY, &h);
    if (err == ESP_OK) {
        err = nvs_get_blob(h, "stock", &stock, &size);
        nvs_close(h);
    }
    bool fresh = err == ESP_ERR_NVS_NOT_FOUND;
    if (err == ESP_OK && size == sizeof stock && stock_valid(&stock)) {
        return;
    }
    memset(&stock, 0, sizeof stock);
    stock.version = STOCK_VERSION;
    if (!fresh) {
        /* Custody is unknown, so the loss goes on record before anything else. */
        journal_add(CRITICAL, "stock", "{\"error\":\"unreadable\",\"size\":%u}", (unsigned)size);
    }
}

bool app_message(node_t from, const char *kind, uint32_t boot, uint32_t seq, const char *type,
                 const char *body)
{
    message_t m = {.from = from, .boot = boot, .seq = seq, .body = strdup(body)};
    strlcpy(m.kind, kind, sizeof m.kind);
    strlcpy(m.type, type, sizeof m.type);
    if (m.body && xQueueSend(inbox, &m, 0) == pdTRUE) {
        return true;
    }
    free(m.body);
    return false;
}

typedef enum { NEED_RECEIVER, NEED_STABLE, NEED_BATTERY, NEED_SAFE_TIME, NEED_LID, NEEDS } need_t;

/* Departure conditions, checked in this order. */
static const struct {
    const char *name, *why;
} departure[NEEDS] = {
    [NEED_RECEIVER] = {"receiver", "receiver not confirmed"},
    [NEED_STABLE] = {"stable", "temperature not stable"},
    [NEED_BATTERY] = {"battery", "battery"},
    [NEED_SAFE_TIME] = {"safe_time", "safe time"},
    [NEED_LID] = {"lid", "lid open"},
};

/* Bit 1 << need is set while that condition is not met. */
static uint32_t departure_unmet(const state_t *s)
{
    uint32_t bits = 0;
    bits |= (uint32_t)!s->receiver_ready << NEED_RECEIVER;
    bits |= (uint32_t)(!s->now.temp_valid || s->stable_s < DEPART_STABLE_S) << NEED_STABLE;
    bits |= (uint32_t)(!s->now.battery_valid || s->now.battery < BATTERY_LOW) << NEED_BATTERY;
    bits |= (uint32_t)(s->safe_s < TRANSPORT_S) << NEED_SAFE_TIME;
    bits |= (uint32_t)s->now.door_open << NEED_LID;
    return bits;
}

/* Lists every unmet condition, so the operator need not try to depart. */
static bool carrier_json(cJSON *out)
{
    state_t s = state_get();
    uint32_t unmet = departure_unmet(&s);
    const char *names[NEEDS];
    int count = 0;
    for (need_t i = 0; i < NEEDS; i++) {
        if (unmet & 1u << i) {
            names[count++] = departure[i].name;
        }
    }
    uint32_t left = s.stable_s < DEPART_STABLE_S ? DEPART_STABLE_S - s.stable_s : 0;
    return cJSON_AddNumberToObject(out, "stable_left_s", left) &&
           cJSON_AddItemToObject(out, "unmet", cJSON_CreateStringArray(names, count));
}

static bool inventory_json(cJSON *out)
{
    xSemaphoreTake(stock_mutex, portMAX_DELAY);
    cJSON *online = cJSON_AddObjectToObject(out, "online");
    cJSON *list = cJSON_AddArrayToObject(out, "batches");
    bool ok = online && list && cJSON_AddNumberToObject(online, "fridge", peer[FRIDGE].online) &&
              cJSON_AddNumberToObject(online, "carrier", peer[CARRIER].online);
    for (int i = 0; ok && i < BATCHES; i++) {
        const batch_t *b = &stock.batch[i];
        const char *row[] = {b->name, places[b->place], phases[b->phase], statuses[b->status]};
        ok = !b->name[0] || cJSON_AddItemToArray(list, cJSON_CreateStringArray(row, 4));
    }
    xSemaphoreGive(stock_mutex);
    return ok;
}

bool app_json(cJSON *out)
{
    switch (self) {
    case CARRIER:
        return carrier_json(out);
    case INVENTORY:
        return inventory_json(out);
    default:
        return true;
    }
}

/* The unit that watches a place, or NODES for none. */
static node_t watcher(place_t place)
{
    switch (place) {
    case IN_FRIDGE:
    case IN_QUARANTINE:
        return FRIDGE;
    case IN_CARRIER:
        return CARRIER;
    default:
        return NODES;
    }
}

static void mark_unusable(stock_t *st, node_t unit)
{
    for (int i = 0; i < BATCHES; i++) {
        batch_t *b = &st->batch[i];
        if (b->name[0] && watcher(b->place) == unit && b->phase != CHECKED_OUT && b->status == OK) {
            b->status = DO_NOT_USE;
            journal_add(CRITICAL, "batch", "{\"batch\":\"%s\",\"status\":\"dnu\",\"place\":\"%s\"}",
                        b->name, places[b->place]);
        }
    }
}

/* Also covers excursions whose event was missed. */
static void follow_count(stock_t *st, node_t unit, uint32_t count)
{
    if (count > st->known_count[unit]) {
        st->known_count[unit] = count;
        mark_unusable(st, unit);
    }
}

static void move_carried(stock_t *st, phase_t from, phase_t to)
{
    for (int i = 0; i < BATCHES; i++) {
        batch_t *b = &st->batch[i];
        if (b->name[0] && b->place == IN_CARRIER && b->phase == from) {
            b->phase = to;
            journal_add(CRITICAL, "batch", "{\"batch\":\"%s\",\"phase\":\"%s\"}", b->name,
                        phases[to]);
        }
    }
}

/* Saved with the batches it moves, so a repeat after a lost save redoes the move. */
static void follow_transit(stock_t *st, uint32_t transit)
{
    if (transit == st->transit) {
        return;
    }
    st->transit = transit;
    if (transit == TRANSIT_ACTIVE) {
        move_carried(st, STORED, IN_TRANSIT);
    } else if (transit == TRANSIT_ARRIVED) {
        move_carried(st, STORED, IN_TRANSIT);
        move_carried(st, IN_TRANSIT, PENDING);
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

/* Batch records come before the save. A failed save leaves stock as it was, and
 * the next repeat of the message redoes the change and its records. */
static void on_peer(const message_t *m, const cJSON *body)
{
    stock_t next = stock;
    if (strcmp(m->kind, "heartbeat") == 0) {
        heard(m->from);
    } else if (strcmp(m->kind, "telemetry") == 0 && strcmp(m->type, "telemetry") == 0) {
        uint32_t valid, excursion, backlog, count, transit = TRANSIT_NONE;
        if (!json_u32(body, "valid", &valid) || !json_u32(body, "excursion", &excursion) ||
            !json_u32(body, "backlog", &backlog) || !json_u32(body, "lock", &count) ||
            (m->from == CARRIER &&
             (!json_u32(body, "transit", &transit) || transit > TRANSIT_ARRIVED))) {
            return;
        }
        peer[m->from].valid = valid;
        peer[m->from].excursion = excursion;
        peer[m->from].backlog = backlog;
        follow_count(&next, m->from, count);
        if (m->from == CARRIER) {
            follow_transit(&next, transit);
        }
    } else if (strcmp(m->kind, "event") == 0) {
        bool newer = m->boot > next.seen_boot[m->from] ||
                     (m->boot == next.seen_boot[m->from] && m->seq > next.seen_seq[m->from]);
        if (!newer) {
            return;
        }
        next.seen_boot[m->from] = m->boot;
        next.seen_seq[m->from] = m->seq;
        uint32_t on, count;
        if (strcmp(m->type, "excursion") == 0 && json_u32(body, "on", &on) &&
            json_u32(body, "lock", &count)) {
            peer[m->from].excursion = on;
            follow_count(&next, m->from, count);
        }
    }
    stock_commit(&next);
}

static batch_t *find(stock_t *st, const char *name)
{
    for (int i = 0; i < BATCHES; i++) {
        if (name[0] && strcmp(st->batch[i].name, name) == 0) {
            return &st->batch[i];
        }
    }
    return NULL;
}

static batch_t *vacant(stock_t *st)
{
    /* Forget a dispensed batch only when needed. */
    for (int reuse = 0; reuse < 2; reuse++) {
        for (int i = 0; i < BATCHES; i++) {
            batch_t *b = &st->batch[i];
            if (!b->name[0] || (reuse && b->phase == CHECKED_OUT)) {
                return b;
            }
        }
    }
    return NULL;
}

/* Why a place cannot vouch for its contents, or NULL. */
static const char *doubt(place_t at)
{
    node_t unit = watcher(at);
    if (unit == NODES) {
        return NULL;
    }
    if (!peer[unit].online) {
        return "storage offline";
    }
    if (!peer[unit].valid) {
        return "sensor fault";
    }
    return peer[unit].excursion ? "excursion active" : NULL;
}

static const char *checkin(stock_t *st, const command_t *c, batch_t *b)
{
    if (b && b->phase != CHECKED_OUT) {
        return "already stored";
    }
    if (!c->name[0]) {
        return "bad name";
    }
    const char *why = doubt(c->to);
    if (why) {
        return why;
    }
    if (!tag[0] || (b && strcmp(b->uid, tag) != 0)) {
        return "scan batch";
    }
    for (int i = 0; i < BATCHES; i++) {
        if (&st->batch[i] != b && st->batch[i].name[0] && strcmp(st->batch[i].uid, tag) == 0) {
            return "tag in use";
        }
    }
    /* A returned batch keeps its verdict. */
    uint8_t status = b ? b->status : OK;
    if (!b && !(b = vacant(st))) {
        return "stock full";
    }
    strlcpy(b->name, c->name, sizeof b->name);
    strlcpy(b->uid, tag, sizeof b->uid);
    b->place = c->to;
    b->phase = STORED;
    b->status = status;
    return NULL;
}

/* Applies the command to st. Returns why it is refused, or NULL. */
static const char *handle_stock(stock_t *st, const command_t *c)
{
    batch_t *b = find(st, c->name);
    if (c->cmd == CMD_CHECKIN) {
        return checkin(st, c, b);
    }
    if (c->cmd >= CMD_RECEIVER) {
        return "unknown command";
    }
    if (!b) {
        return "unknown batch";
    }
    if (strcmp(b->uid, tag) != 0) {
        return "scan batch";
    }
    const char *why;
    switch (c->cmd) {
    case CMD_QUARANTINE:
        if (b->status != DO_NOT_USE || b->place != IN_FRIDGE || b->phase != STORED) {
            return "not locked in fridge";
        }
        b->place = IN_QUARANTINE;
        return NULL;
    case CMD_RELEASE:
    case CMD_DISCARD:
        if (!c->note[0]) {
            return "basis required";
        }
        if (b->status != DO_NOT_USE) {
            return "not locked";
        }
        why = c->cmd == CMD_RELEASE && b->phase != CHECKED_OUT ? doubt(b->place) : NULL;
        if (why) {
            return why;
        }
        if (c->cmd == CMD_DISCARD) {
            b->status = DISCARDED;
            b->place = IN_DISCARD;
            b->phase = STORED;
        } else {
            b->status = OK;
            if (b->place == IN_QUARANTINE) {
                b->place = IN_FRIDGE;
            }
        }
        return NULL;
    default:
        break;
    }
    if (b->status != OK) {
        return b->status == DO_NOT_USE ? "do not use" : "discarded";
    }
    if (c->cmd == CMD_ACCEPT) {
        if (b->phase != PENDING) {
            return "not pending";
        }
        why = doubt(IN_CARRIER);
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
    why = b->phase == CHECKED_OUT ? NULL : doubt(b->place);
    if (why) {
        return why;
    }
    if (b->phase != STORED) {
        return phases[b->phase];
    }
    if (c->cmd == CMD_CHECKOUT) {
        b->phase = CHECKED_OUT;
        return NULL;
    }
    /* CMD_LOAD */
    if (b->place != IN_FRIDGE) {
        return "not in fridge";
    }
    why = doubt(IN_CARRIER);
    if (why) {
        return why;
    }
    if (st->transit != TRANSIT_NONE) {
        return "carrier away";
    }
    b->place = IN_CARRIER;
    return NULL;
}

/* Sets the transport state the command leads to. Returns why it is refused, or NULL. */
static const char *handle_carrier(const command_t *c, const state_t *s, uint8_t *transit,
                                  bool *receiver_ready)
{
    *transit = s->transit;
    *receiver_ready = false;
    switch (c->cmd) {
    case CMD_RECEIVER:
        *receiver_ready = true;
        return NULL;
    case CMD_DEPART: {
        if (s->transit != TRANSIT_NONE) {
            return "already departed";
        }
        uint32_t unmet = departure_unmet(s);
        if (unmet) {
            return departure[__builtin_ctz(unmet)].why;
        }
        *transit = TRANSIT_ACTIVE;
        return NULL;
    }
    case CMD_ARRIVE:
        if (s->transit != TRANSIT_ACTIVE) {
            return "not in transit";
        }
        *transit = TRANSIT_ARRIVED;
        return NULL;
    case CMD_RETURN:
        if (s->transit != TRANSIT_ARRIVED) {
            return "not at receiver";
        }
        *transit = TRANSIT_NONE;
        return NULL;
    default:
        return "unknown command";
    }
}

/* Text that goes into a record without escaping. */
static bool plain(const char *s, char *out, size_t size)
{
    size_t n = strlen(s);
    for (size_t i = 0; i < n; i++) {
        if ((unsigned char)s[i] < 0x20 || s[i] == '"' || s[i] == '\\') {
            return false;
        }
    }
    return strlcpy(out, s, size) < size;
}

static const char *text(const cJSON *object, const char *key)
{
    const char *s = cJSON_GetStringValue(cJSON_GetObjectItemCaseSensitive(object, key));
    return s ? s : "";
}

/* Returns false for a command that must be ignored, not refused. */
static bool parse_command(const cJSON *body, command_t *c)
{
    if (!plain(text(body, "cmd"), c->word, sizeof c->word) ||
        !plain(text(body, "batch"), c->name, sizeof c->name)) {
        return false;
    }
    c->bad_note = !plain(text(body, "note"), c->note, sizeof c->note);
    if (c->bad_note) {
        c->note[0] = '\0';
    }
    c->cmd = 0;
    while (c->cmd < CMD_UNKNOWN && strcmp(c->word, commands[c->cmd]) != 0) {
        c->cmd++;
    }
    c->to = strcmp(text(body, "to"), "carrier") == 0 ? IN_CARRIER : IN_FRIDGE;
    return true;
}

static void refuse(const command_t *c, const state_t *s, const char *why)
{
    journal_add(CRITICAL, "op",
                "{\"cmd\":\"%s\",\"batch\":\"%s\",\"ok\":0,\"person\":\"%s\",\"why\":\"%s\"}",
                c->word, c->name, s->person, why);
}

/* The record comes first and the state follows. A failed save adds a refusal
 * after the record. A crash between the two leaves a record without its effect.
 * State stays authoritative and the operator repeats the command. */
static void on_command(const message_t *m, const cJSON *body)
{
    /* Delivery may repeat. The host numbers commands upward. */
    static uint32_t boot, seq;
    if (m->boot < boot || (m->boot == boot && m->seq <= seq)) {
        return;
    }
    boot = m->boot;
    seq = m->seq;
    command_t c;
    if (!parse_command(body, &c)) {
        return;
    }
    state_t s = state_get();
    if (!s.person[0]) {
        refuse(&c, &s, "badge required");
        return;
    }
    if (c.bad_note) {
        refuse(&c, &s, "bad note");
        return;
    }
    if (journal_full()) {
        refuse(&c, &s, "log full");
        return;
    }
    if (self == INVENTORY) {
        if (now_ms() - tag_ms > BADGE_MS) {
            tag[0] = '\0';
        }
        stock_t next = stock;
        const char *why = handle_stock(&next, &c);
        if (why) {
            refuse(&c, &s, why);
        } else if (
            journal_add(
                CRITICAL, "op",
                "{\"cmd\":\"%s\",\"batch\":\"%s\",\"ok\":1,\"person\":\"%s\",\"note\":\"%s\"}",
                c.word, c.name, s.person, c.note) &&
            !stock_commit(&next)) {
            refuse(&c, &s, "storage");
        }
    } else if (self == CARRIER) {
        uint8_t transit;
        bool ready;
        const char *why = handle_carrier(&c, &s, &transit, &ready);
        if (why) {
            refuse(&c, &s, why);
        } else if (journal_add(CRITICAL, "op",
                               "{\"cmd\":\"%s\",\"ok\":1,\"person\":\"%s\",\"temp_c\":%.2f,"
                               "\"battery\":%.1f,\"box\":\"BOX-1\",\"note\":\"%s\"}",
                               c.word, s.person, s.now.temp_c, s.now.battery * 100, c.note) &&
                   !state_transit(transit, ready)) {
            refuse(&c, &s, "storage");
        }
    } else {
        refuse(&c, &s, "unknown command");
    }
}

static void application(void *arg)
{
    for (;;) {
        message_t m;
        if (xQueueReceive(inbox, &m, pdMS_TO_TICKS(500)) == pdTRUE) {
            cJSON *body = cJSON_Parse(m.body);
            xSemaphoreTake(stock_mutex, portMAX_DELAY);
            if (body && strcmp(m.kind, "command") == 0) {
                on_command(&m, body);
            } else if (body && self == INVENTORY) {
                on_peer(&m, body);
            }
            xSemaphoreGive(stock_mutex);
            cJSON_Delete(body);
            free(m.body);
        }
        if (self == INVENTORY) {
            xSemaphoreTake(stock_mutex, portMAX_DELAY);
            watch_peers();
            xSemaphoreGive(stock_mutex);
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
                xSemaphoreTake(stock_mutex, portMAX_DELAY);
                strlcpy(tag, uid, sizeof tag);
                tag_ms = now_ms();
                xSemaphoreGive(stock_mutex);
            }
            journal_add(CRITICAL, "scan", "{\"uid\":\"%s\",\"staff\":%d}", uid, staff);
        } else {
            journal_add(CRITICAL, "scan", "{\"error\":\"bad frame\"}");
        }
    }
}

void app_start(void)
{
    MUST(inbox = xQueueCreate(16, sizeof(message_t)));
    MUST(stock_mutex = xSemaphoreCreateMutex());
    stock_load();
    MUST(xTaskCreate(application, "app", 6144, NULL, 5, NULL) == pdPASS);
    MUST(xTaskCreate(badges, "badge", 4096, NULL, 6, NULL) == pdPASS);
}
