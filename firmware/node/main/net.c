/* Wi-Fi, MQTT envelopes, heartbeat and event delivery. */

#include "node.h"

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

enum { CONNECTED = BIT0, BEAT = BIT1, SAMPLES = BIT2 };

static esp_mqtt_client_handle_t client;
static EventGroupHandle_t flags;

static void topic(char *out, size_t size, const char *node, const char *kind)
{
    snprintf(out, size, PREFIX "%s/%s", node, kind);
}

static bool send(const char *kind, const char *type, uint32_t boot, uint32_t seq, int64_t time_ms,
                 const char *body)
{
    static char text[2048], escaped[1536];
    char *e = escaped;
    for (const char *c = body; *c && e < escaped + sizeof escaped - 3; c++) {
        if (*c == '"' || *c == '\\') {
            *e++ = '\\';
        }
        *e++ = *c;
    }
    *e = '\0';
    uint32_t crc = esp_rom_crc32_le(0, (const uint8_t *)body, strlen(body));
    int n = snprintf(text, sizeof text,
                     "{\"version\":1,\"node_id\":\"%s\",\"boot_id\":%lu,\"seq\":%lu,"
                     "\"sim_time_ms\":%lld,\"type\":\"%s\",\"body\":\"%s\",\"checksum\":%lu}",
                     node_names[self], (unsigned long)boot, (unsigned long)seq, (long long)time_ms,
                     type, escaped, (unsigned long)crc);
    if (n >= (int)sizeof text) {
        return false;
    }
    char name[64];
    topic(name, sizeof name, node_names[self], kind);
    bool durable = strcmp(kind, "event") == 0;
    /* Events wait in flash, so the client must not hold the caller. */
    int id = durable ? esp_mqtt_client_enqueue(client, name, text, n, 1, 0, true)
                     : esp_mqtt_client_publish(client, name, text, n, 0, 0);
    return id >= 0;
}

static void telemetry(void)
{
    state_t s = state_get();
    char body[1024];
    int n = snprintf(body, sizeof body,
                     "{\"temp_c\":%.3f,\"valid\":%d,\"battery\":%.2f,\"battery_mv\":%u,\"door\":%d,"
                     "\"mains\":%d,\"relay\":%d,\"lock\":%lu,\"excursion\":%d,\"safe_s\":%ld,"
                     "\"transit\":%d,\"receiver\":%d,\"person\":\"%s\",\"backlog\":%d,\"alarms\":[",
                     s.now.temp_c, s.now.temp_valid, s.now.battery * 100, s.now.battery_mv,
                     s.now.door_open, s.now.mains, s.relay, (unsigned long)s.lock, s.excursion,
                     (long)s.safe_s, s.transit, s.receiver_ready,
                     s.person[0] && now_ms() - s.person_ms < BADGE_MS ? s.person : "",
                     journal_backlog());
    bool first = true;
    for (int i = 0; i < ALARMS; i++) {
        if (s.alarms & 1u << i) {
            n += snprintf(body + n, sizeof body - n, "%s\"%s\"", first ? "" : ",", alarm_names[i]);
            first = false;
        }
    }
    n += snprintf(body + n, sizeof body - n, "]");
    if (self == INVENTORY) {
        n += app_stock_json(body + n, sizeof body - n - 2);
    }
    snprintf(body + n, sizeof body - n, "}");
    send("telemetry", "telemetry", journal_boot(), journal_seq(), now_ms(), body);
}

static void samples(void)
{
    float t[60];
    char body[640] = "[";
    int count = control_recent(t), n = 1;
    for (int i = 60 - count; i < 60; i++) {
        n += snprintf(body + n, sizeof body - n, "%s%.2f", n > 1 ? "," : "", t[i]);
    }
    snprintf(body + n, sizeof body - n, "]");
    send("telemetry", "samples", journal_boot(), journal_seq(), now_ms(), body);
}

/* Sends the oldest records first. Unconfirmed ones repeat. */
static void deliver(void)
{
    static struct {
        uint32_t boot, seq;
        int64_t at;
    } sent[WINDOW];
    record_t r;
    int slot;
    for (int i = 0; i < WINDOW && journal_pending(i, &r, &slot); i++) {
        int free = 0;
        bool waiting = false;
        for (int k = 0; k < WINDOW; k++) {
            if (sent[k].boot == r.boot && sent[k].seq == r.seq) {
                waiting = now_ms() - sent[k].at < RESEND_MS;
                free = k;
                break;
            }
            if (sent[k].at < sent[free].at) {
                free = k;
            }
        }
        if (!waiting && send("event", r.type, r.boot, r.seq, r.time_ms, r.body)) {
            sent[free].boot = r.boot;
            sent[free].seq = r.seq;
            sent[free].at = now_ms();
        }
    }
}

static void communicate(void *arg)
{
    int64_t next = 0;
    for (;;) {
        EventBits_t bits =
            xEventGroupWaitBits(flags, BEAT | SAMPLES, pdTRUE, pdFALSE, pdMS_TO_TICKS(100));
        if (!(xEventGroupGetBits(flags) & CONNECTED)) {
            continue;
        }
        if (bits & BEAT) {
            send("heartbeat", "heartbeat", journal_boot(), journal_seq(), now_ms(), "{}");
        }
        if (bits & SAMPLES) {
            samples();
        }
        if (now_ms() >= next) {
            next = now_ms() + 1000;
            telemetry();
        }
        deliver();
    }
}

void net_samples(void)
{
    if (flags) {
        xEventGroupSetBits(flags, SAMPLES);
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

static uint32_t number(const cJSON *object, const char *key)
{
    const cJSON *n = cJSON_GetObjectItemCaseSensitive(object, key);
    return cJSON_IsNumber(n) && n->valuedouble >= 0 ? (uint32_t)n->valuedouble : 0;
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
    node_t from = 0;
    while (from < NODES && strcmp(node, node_names[from]) != 0) {
        from++;
    }
    if (from == NODES) {
        return;
    }
    cJSON *m = cJSON_ParseWithLength(data, len);
    const char *body = text(m, "body"), *type = text(m, "type"), *sender = text(m, "node_id");
    bool own = strcmp(kind, "command") == 0 || strcmp(kind, "ack") == 0;
    if (m && body && type && sender && number(m, "version") == 1
        && strcmp(sender, own ? "host" : node) == 0
        && number(m, "checksum") == esp_rom_crc32_le(0, (const uint8_t *)body, strlen(body))) {
        if (strcmp(kind, "ack") == 0) {
            cJSON *b = cJSON_Parse(body);
            journal_ack(number(b, "boot_id"), number(b, "seq"));
            cJSON_Delete(b);
        } else {
            app_message(from, kind, number(m, "boot_id"), number(m, "seq"), type, body);
        }
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
    } else if (base == WIFI_EVENT && (id == WIFI_EVENT_STA_START || id == WIFI_EVENT_STA_DISCONNECTED)) {
        esp_wifi_connect();
    }
}

void net_start(void)
{
    if (!broker_uri[0]) {
        return;
    }
    flags = xEventGroupCreate();
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
    client = esp_mqtt_client_init(&config);
    ESP_ERROR_CHECK(esp_mqtt_client_register_event(client, MQTT_EVENT_ANY, on_mqtt, NULL));

    wifi_config_t sta = {.sta = {.ssid = "myssid", .password = "mypassword"}};
    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &sta));
    ESP_ERROR_CHECK(esp_wifi_set_ps(WIFI_PS_NONE));
    ESP_ERROR_CHECK(esp_wifi_start());

    TimerHandle_t timer = xTimerCreate("beat", pdMS_TO_TICKS(5000), pdTRUE, NULL, beat);
    xTimerStart(timer, portMAX_DELAY);
    xTaskCreate(communicate, "net", 8192, NULL, 3, NULL);
}
