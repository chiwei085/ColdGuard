#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

/* Boot time resources. A node without them cannot run, so it restarts. */
#define MUST(ok) ESP_ERROR_CHECK((ok) ? ESP_OK : ESP_ERR_NO_MEM)

typedef enum { FRIDGE, CARRIER, INVENTORY, NODES } node_t;

extern const char *const node_names[NODES];
extern node_t self;
/* Returns NODES for an unknown name. */
node_t node_find(const char *name);
extern char broker_uri[64];

#define BODY_MAX 216
#define UID_LEN 8
/* A badge authorizes operations at its node for this long. */
#define BADGE_MS 60000
/* Temperatures kept for the rise estimate and the samples message. */
#define HISTORY_S 60
/* Packing, travel and margin. */
#define TRANSPORT_S ((5 + 15 + 15) * 60)
#define BATTERY_LOW 0.2f

/* Alarm bits. Names are in control.c. */
enum {
    ALARM_DOOR,
    ALARM_EXCURSION,
    ALARM_FREEZE,
    ALARM_SENSOR,
    ALARM_COOLING,
    ALARM_MAINS,
    ALARM_BATTERY_LOW,
    ALARM_BATTERY_CRITICAL,
    ALARM_TRANSPORT,
    ALARM_LOG,
    ALARMS,
};

extern const char *const alarm_names[ALARMS];

enum { TRANSIT_NONE, TRANSIT_ACTIVE, TRANSIT_ARRIVED };

/* One second of readings. */
typedef struct {
    int64_t at_ms;
    bool temp_valid;
    float temp_c;
    bool battery_valid;
    float battery;
    uint16_t battery_mv;
    bool door_open;
    bool mains;
} sample_t;

typedef struct {
    sample_t now;
    bool relay;
    uint32_t alarms;
    /* Never falls, so a missed excursion event is still noticed. */
    uint32_t excursion_count;
    bool excursion;
    /* Seconds of safe storage left. Negative means unknown. */
    int32_t safe_s;
    /* Consecutive seconds inside the safe range. */
    uint32_t stable_s;
    uint32_t door_s;
    uint8_t transit;
    bool receiver_ready;
    char person[UID_LEN + 1];
    int64_t person_ms;
} state_t;

/* Battery clock time. Uptime when the clock cannot be read. */
int64_t now_ms(void);

/* Safe from any task. The person is empty once the badge has expired. */
state_t state_get(void);
/* Returns false when it could not be persisted. Nothing changes then. */
bool state_transit(uint8_t transit, bool receiver_ready);
void state_person(const char *uid);

/* Boots the buses. Sampling and tag reading never block on the network. */
void bus_init(void);
void bus_sample(sample_t *out);
void bus_relay(bool on);
void bus_buzzer(bool on);
/* Waits for a frame. Returns false and an empty string for a corrupt one. */
bool bus_tag(char uid[UID_LEN + 1]);

void control_start(void);

typedef enum { SUMMARY, CRITICAL } kind_t;

typedef struct {
    uint32_t boot;
    uint32_t seq;
    int64_t time_ms;
    kind_t kind;
    char type[12];
    char body[BODY_MAX];
} record_t;

void journal_init(void);
/* Returns after the record is in flash. */
bool journal_add(kind_t kind, const char *type, const char *fmt, ...)
    __attribute__((format(printf, 3, 4)));
/* Queued, for tasks that must not wait for flash. A lost record is itself recorded later. */
void journal_post(kind_t kind, const char *type, const char *fmt, ...)
    __attribute__((format(printf, 3, 4)));
/* Reads the oldest unacknowledged records. Returns the count. */
int journal_pending(record_t *out, int max);
void journal_ack(uint32_t boot, uint32_t seq);
int journal_backlog(void);
/* Critical records cannot be stored now, for lack of space or a flash fault. */
bool journal_full(void);
/* Full, summaries have stopped to keep the critical reserve, or a record was lost. */
bool journal_low(void);
uint32_t journal_boot(void);
uint32_t journal_seq(void);

struct cJSON;

void net_start(void);
/* False unless the key holds an integer in uint32_t range. */
bool json_u32(const struct cJSON *object, const char *key, uint32_t *out);

void app_start(void);
/* Called from the MQTT task. Takes a copy. Returns false when it was dropped. */
bool app_message(node_t from, const char *kind, uint32_t boot, uint32_t seq, const char *type,
                 const char *body);
/* Adds the node specific part of the telemetry body. Returns false when out of memory. */
bool app_json(struct cJSON *body);
