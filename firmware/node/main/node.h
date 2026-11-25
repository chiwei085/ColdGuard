#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

typedef enum { FRIDGE, CARRIER, INVENTORY, NODES } node_t;

extern const char *const node_names[NODES];
extern node_t self;
extern char broker_uri[64];

#define BODY_MAX 216
#define UID_LEN 8
/* A badge authorizes operations at its node for this long. */
#define BADGE_MS 60000

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
    /* Counts excursions. Never falls, so a missed event is still noticed. */
    uint32_t lock;
    bool excursion;
    /* Seconds of safe storage left. Negative means unknown. */
    int32_t safe_s;
    /* Consecutive seconds inside the safe range. */
    uint32_t stable_s;
    uint8_t transit;
    bool receiver_ready;
    char person[UID_LEN + 1];
    int64_t person_ms;
} state_t;

int64_t now_ms(void);

/* Safe from any task. */
state_t state_get(void);
void state_transit(uint8_t transit, bool receiver_ready);
void state_person(const char *uid);

/* Boots the buses. Sampling and tag reading never block on the network. */
void bus_init(void);
bool bus_sample(sample_t *out);
void bus_relay(bool on);
void bus_buzzer(bool on);
/* Waits for a frame. Returns false and an empty string for a corrupt one. */
bool bus_tag(char uid[UID_LEN + 1]);

void control_start(void);
/* Last minute of temperatures, oldest first. Returns the count. */
int control_recent(float out[60]);

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
/* Queued, for tasks that must not wait for flash. */
void journal_post(kind_t kind, const char *type, const char *fmt, ...)
    __attribute__((format(printf, 3, 4)));
/* Reads the n-th unacknowledged record. */
bool journal_pending(int n, record_t *out, int *slot);
void journal_ack(uint32_t boot, uint32_t seq);
int journal_backlog(void);
bool journal_full(void);
uint32_t journal_boot(void);
uint32_t journal_seq(void);

void net_start(void);
void net_samples(void);

void app_start(void);
/* Called from the MQTT task. Takes a copy. */
void app_message(node_t from, const char *kind, uint32_t boot, uint32_t seq, const char *type,
                 const char *body);
/* Inventory part of the telemetry body. */
int app_stock_json(char *out, int size);
