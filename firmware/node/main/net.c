/* Wi-Fi, MQTT envelopes, heartbeat and event delivery. */

#include "node.h"

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cJSON.h"
#include "esp_event.h"
#include "esp_netif.h"
#include "esp_rom_crc.h"
#include "esp_wifi.h"
#include "freertos/event_groups.h"
#include "freertos/timers.h"
#include "mqtt_client.h"

#define PREFIX "coldguard/v1/"
#define WINDOW 8
#define RESEND_MS 5000

enum { CONNECTED = BIT0, BEAT = BIT1 };

static esp_mqtt_client_handle_t client;
static EventGroupHandle_t flags;

static void topic(char *out, size_t size, const char *node, const char *kind)
{
    snprintf(out, size, PREFIX "%s/%s", node, kind);
}

/* Rounds for the wire, so floats print short. */
static double fixed(double value, double scale)
{
    return round(value * scale) / scale;
}

static bool send(const char *kind, const char *type, uint32_t boot, uint32_t seq, int64_t time_ms,
                 const char *body)
{
    static char text[2048];
    uint32_t crc = esp_rom_crc32_le(0, (const uint8_t *)body, strlen(body));
    cJSON *m = cJSON_CreateObject();
    /* Printing fails rather than truncate when the buffer is short. */
    bool built =
        m && cJSON_AddNumberToObject(m, "version", 1) &&
        cJSON_AddStringToObject(m, "node_id", node_names[self]) &&
        cJSON_AddNumberToObject(m, "boot_id", boot) && cJSON_AddNumberToObject(m, "seq", seq) &&
        cJSON_AddNumberToObject(m, "sim_time_ms", (double)time_ms) &&
        cJSON_AddStringToObject(m, "type", type) && cJSON_AddStringToObject(m, "body", body) &&
        cJSON_AddNumberToObject(m, "checksum", crc) &&
        cJSON_PrintPreallocated(m, text, sizeof text, false);
    cJSON_Delete(m);
    if (!built) {
        return false;
    }
    char name[64];
    topic(name, sizeof name, node_names[self], kind);
    int qos = strcmp(kind, "event") == 0;
    /* Queued messages leave one per client loop, which starves the backlog. */
    int id = esp_mqtt_client_publish(client, name, text, strlen(text), qos, 0);
    return id >= 0;
}

static bool telemetry_body(cJSON *b, const state_t *s)
{
    const char *alarms[ALARMS];
    int count = 0;
    for (int i = 0; i < ALARMS; i++) {
        if (s->alarms & 1u << i) {
            alarms[count++] = alarm_names[i];
        }
    }
    const sample_t *n = &s->now;
    return cJSON_AddNumberToObject(b, "temp_c", fixed(n->temp_c, 1000)) &&
           cJSON_AddNumberToObject(b, "valid", n->temp_valid) &&
           cJSON_AddNumberToObject(b, "battery", fixed(n->battery * 100, 100)) &&
           cJSON_AddNumberToObject(b, "battery_mv", n->battery_mv) &&
           cJSON_AddNumberToObject(b, "door", n->door_open) &&
           cJSON_AddNumberToObject(b, "mains", n->mains) &&
           cJSON_AddNumberToObject(b, "relay", s->relay) &&
           cJSON_AddNumberToObject(b, "lock", s->excursion_count) &&
           cJSON_AddNumberToObject(b, "excursion", s->excursion) &&
           cJSON_AddNumberToObject(b, "safe_s", s->safe_s) &&
           cJSON_AddNumberToObject(b, "transit", s->transit) &&
           cJSON_AddNumberToObject(b, "receiver", s->receiver_ready) &&
           cJSON_AddStringToObject(b, "person", s->person) &&
           cJSON_AddNumberToObject(b, "backlog", journal_backlog()) &&
           cJSON_AddNumberToObject(b, "door_s", s->door_s) &&
           cJSON_AddItemToObject(b, "alarms", cJSON_CreateStringArray(alarms, count)) &&
           app_json(b);
}

static void telemetry(void)
{
    state_t s = state_get();
    char text[1024];
    cJSON *b = cJSON_CreateObject();
    bool built = b && telemetry_body(b, &s) && cJSON_PrintPreallocated(b, text, sizeof text, false);
    cJSON_Delete(b);
    if (built) {
        send("telemetry", "telemetry", journal_boot(), journal_seq(), now_ms(), text);
    }
}

/* Sends the oldest records first. Unconfirmed ones repeat. */
static void deliver(void)
{
    static struct {
        uint32_t boot, seq;
        int64_t at;
    } sent[WINDOW];
    static record_t batch[WINDOW];
    int count = journal_pending(batch, WINDOW);
    for (int i = 0; i < count; i++) {
        const record_t *r = &batch[i];
        int free = 0;
        bool waiting = false;
        for (int k = 0; k < WINDOW; k++) {
            if (sent[k].boot == r->boot && sent[k].seq == r->seq) {
                waiting = now_ms() - sent[k].at < RESEND_MS;
                free = k;
                break;
            }
            if (sent[k].at < sent[free].at) {
                free = k;
            }
        }
        if (!waiting && send("event", r->type, r->boot, r->seq, r->time_ms, r->body)) {
            sent[free].boot = r->boot;
            sent[free].seq = r->seq;
            sent[free].at = now_ms();
        }
    }
}

static void communicate(void *arg)
{
    int64_t next = 0;
    for (;;) {
        EventBits_t bits = xEventGroupWaitBits(flags, BEAT, pdTRUE, pdFALSE, pdMS_TO_TICKS(100));
        if (!(xEventGroupGetBits(flags) & CONNECTED)) {
            continue;
        }
        if (bits & BEAT) {
            send("heartbeat", "heartbeat", journal_boot(), journal_seq(), now_ms(), "{}");
        }
        if (now_ms() >= next) {
            next = now_ms() + 1000;
            telemetry();
        }
        deliver();
    }
}

static void beat(TimerHandle_t timer)
{
    xEventGroupSetBits(flags, BEAT);
}

static const char *text(const cJSON *object, const char *key)
{
    return cJSON_GetStringValue(cJSON_GetObjectItemCaseSensitive(object, key));
}

bool json_u32(const cJSON *object, const char *key, uint32_t *out)
{
    const cJSON *n = cJSON_GetObjectItemCaseSensitive(object, key);
    double v = cJSON_IsNumber(n) ? n->valuedouble : -1;
    /* Written so NaN fails too. */
    if (!(v >= 0 && v <= UINT32_MAX && v == floor(v))) {
        return false;
    }
    *out = (uint32_t)v;
    return true;
}

static void receive(const char *name, int name_len, const char *data, int len)
{
    char path[64];
    if (name_len <= (int)strlen(PREFIX) || name_len >= (int)sizeof path) {
        return;
    }
    memcpy(path, name, name_len);
    path[name_len] = '\0';
    char *node = path + strlen(PREFIX);
    char *kind = strchr(node, '/');
    if (!kind) {
        return;
    }
    *kind++ = '\0';
    node_t from = node_find(node);
    bool own = strcmp(kind, "command") == 0 || strcmp(kind, "ack") == 0;
    /* The wildcard subscriptions also return the unit's own messages. */
    if (from == NODES || (!own && from == self)) {
        return;
    }
    cJSON *m = cJSON_ParseWithLength(data, len);
    const char *body = text(m, "body"), *type = text(m, "type"), *sender = text(m, "node_id");
    uint32_t version, checksum, boot, seq;
    bool valid = m && body && type && sender && strcmp(sender, own ? "host" : node) == 0 &&
                 json_u32(m, "version", &version) && version == 1 &&
                 json_u32(m, "checksum", &checksum) &&
                 checksum == esp_rom_crc32_le(0, (const uint8_t *)body, strlen(body)) &&
                 json_u32(m, "boot_id", &boot) && json_u32(m, "seq", &seq);
    if (valid && strcmp(kind, "ack") == 0) {
        cJSON *b = cJSON_Parse(body);
        if (json_u32(b, "boot_id", &boot) && json_u32(b, "seq", &seq)) {
            journal_ack(boot, seq);
        }
        cJSON_Delete(b);
    } else if (valid && !app_message(from, kind, boot, seq, type, body)) {
        journal_post(CRITICAL, "drop", "{\"kind\":\"%s\",\"seq\":%lu}", kind, (unsigned long)seq);
    }
    cJSON_Delete(m);
}

static void subscribe(const char *node, const char *kind)
{
    char name[64];
    topic(name, sizeof name, node, kind);
    esp_mqtt_client_subscribe_single(client, name, 1);
}

static void on_mqtt(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    esp_mqtt_event_handle_t e = data;
    switch (id) {
    case MQTT_EVENT_CONNECTED:
        subscribe(node_names[self], "command");
        subscribe(node_names[self], "ack");
        if (self == INVENTORY) {
            subscribe("+", "event");
            subscribe("+", "heartbeat");
            subscribe("+", "telemetry");
        }
        xEventGroupSetBits(flags, CONNECTED | BEAT);
        break;
    case MQTT_EVENT_DISCONNECTED:
        xEventGroupClearBits(flags, CONNECTED);
        break;
    case MQTT_EVENT_DATA:
        if (e->data_len == e->total_data_len) {
            receive(e->topic, e->topic_len, e->data, e->data_len);
        }
        break;
    default:
        break;
    }
}

static void on_wifi(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        static bool started;
        if (!started) {
            started = true;
            esp_mqtt_client_start(client);
        }
    } else if (base == WIFI_EVENT &&
               (id == WIFI_EVENT_STA_START || id == WIFI_EVENT_STA_DISCONNECTED)) {
        esp_wifi_connect();
    }
}

void net_start(void)
{
    if (!broker_uri[0]) {
        return;
    }
    MUST(flags = xEventGroupCreate());
    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    esp_netif_create_default_wifi_sta();
    wifi_init_config_t init = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&init));
    ESP_ERROR_CHECK(esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, on_wifi, NULL));
    ESP_ERROR_CHECK(esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, on_wifi, NULL));

    /* The broker keeps a session so missed events arrive after a reconnect. */
    esp_mqtt_client_config_t config = {
        .broker.address.uri = broker_uri,
        .credentials.client_id = node_names[self],
        .session.disable_clean_session = true,
        .session.keepalive = 60,
        .network.reconnect_timeout_ms = 2000,
        .buffer.size = 2048,
    };
    MUST(client = esp_mqtt_client_init(&config));
    ESP_ERROR_CHECK(esp_mqtt_client_register_event(client, MQTT_EVENT_ANY, on_mqtt, NULL));

    wifi_config_t sta = {.sta = {.ssid = "myssid", .password = "mypassword"}};
    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &sta));
    ESP_ERROR_CHECK(esp_wifi_set_ps(WIFI_PS_NONE));
    ESP_ERROR_CHECK(esp_wifi_start());

    TimerHandle_t timer;
    MUST(timer = xTimerCreate("beat", pdMS_TO_TICKS(5000), pdTRUE, NULL, beat));
    MUST(xTimerStart(timer, portMAX_DELAY) == pdPASS);
    MUST(xTaskCreate(communicate, "net", 8192, NULL, 3, NULL) == pdPASS);
}
