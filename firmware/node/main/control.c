/* Sampling, cooling control and alarms. Independent of the network. */

#include "node.h"

#include <math.h>
#include <stdio.h>
#include <string.h>

#include "esp_task_wdt.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "nvs.h"

#define SAFE_LOW_C 2.0f
#define SAFE_HIGH_C 8.0f
#define FREEZE_C 0.0f
#define COOL_ON_C 5.5f
#define COOL_OFF_C 4.5f
#define DOOR_LIMIT_S 60
#define COOLING_WINDOW_S 300
#define COOLING_DROP_C 0.2f
/* Below four sensor steps a rise is indistinguishable from noise. */
#define MIN_RISE_C 0.031f
/* Battery capacity over monitoring load. */
#define BATTERY_FULL_S 36000
#define SAMPLE_MS 1000
/* A longer silence breaks every run that needs consecutive readings. */
#define GAP_MS 2500
/* Passive carrier, validated model at high ambient. */
#define CARRIER_TAU_S 100000.0f
#define CARRIER_AMBIENT_C 40.0f

const char *const alarm_names[ALARMS] = {
    "door_open",  "excursion",   "freeze",           "sensor",    "cooling",
    "mains_lost", "battery_low", "battery_critical", "transport", "log_full",
};

/* Guarded by state_mutex. */
static state_t state;

/* Sample history behind the alarms. Owned by the control task. Times are
 * the start of the first sample in a run, or 0 outside one. */
static struct {
    int64_t last_ms;
    float recent[HISTORY_S];
    int recent_count;
    bool rise_known;
    int64_t stable_since, door_since, invalid_since;
    int64_t excursion_since;
    float excursion_low, excursion_high;
    int64_t cooling_since;
    float cooling_start_c;
    int64_t minute_since;
    int minute_count;
    float minute_low, minute_high, minute_sum;
    /* What NVS holds, so a failed write is retried. */
    uint32_t saved_count;
    bool saved_excursion;
} ctl;

static SemaphoreHandle_t state_mutex;
static QueueHandle_t samples;

state_t state_get(void)
{
    xSemaphoreTake(state_mutex, portMAX_DELAY);
    state_t copy = state;
    xSemaphoreGive(state_mutex);
    if (now_ms() - copy.person_ms >= BADGE_MS) {
        copy.person[0] = '\0';
    }
    return copy;
}

static bool save(const char *key, uint32_t value)
{
    nvs_handle_t h;
    if (nvs_open("node", NVS_READWRITE, &h) != ESP_OK) {
        return false;
    }
    bool ok = nvs_set_u32(h, key, value) == ESP_OK && nvs_commit(h) == ESP_OK;
    nvs_close(h);
    return ok;
}

static uint32_t restore(const char *key)
{
    nvs_handle_t h;
    uint32_t value = 0;
    if (nvs_open("node", NVS_READONLY, &h) == ESP_OK) {
        nvs_get_u32(h, key, &value);
        nvs_close(h);
    }
    return value;
}

bool state_transit(uint8_t transit, bool receiver_ready)
{
    if (!save("transit", transit)) {
        return false;
    }
    xSemaphoreTake(state_mutex, portMAX_DELAY);
    state.transit = transit;
    state.receiver_ready = receiver_ready;
    xSemaphoreGive(state_mutex);
    return true;
}

void state_person(const char *uid)
{
    xSemaphoreTake(state_mutex, portMAX_DELAY);
    strlcpy(state.person, uid, sizeof state.person);
    state.person_ms = now_ms();
    xSemaphoreGive(state_mutex);
}

/* The minute before an excursion, oldest first, in records that fit a slot. */
static void evidence(void)
{
    enum { PER_RECORD = 30 };
    for (int first = 0; first < ctl.recent_count; first += PER_RECORD) {
        int last = first + PER_RECORD < ctl.recent_count ? first + PER_RECORD : ctl.recent_count;
        char list[PER_RECORD * 7] = "";
        int n = 0;
        for (int i = first; i < last; i++) {
            int offset = HISTORY_S - ctl.recent_count + i;
            n += snprintf(list + n, sizeof list - n, "%s%d", i > first ? "," : "",
                          (int)lroundf(ctl.recent[offset] * 100));
        }
        journal_post(CRITICAL, "samples", "{\"from_s\":%d,\"centi_c\":[%s]}",
                     first - ctl.recent_count, list);
    }
}

static void sampler(void *arg)
{
    esp_task_wdt_add(NULL);
    TickType_t wake = xTaskGetTickCount();
    for (;;) {
        sample_t s;
        bus_sample(&s);
        s.at_ms = now_ms();
        xQueueOverwrite(samples, &s);
        esp_task_wdt_reset();
        vTaskDelayUntil(&wake, pdMS_TO_TICKS(SAMPLE_MS));
    }
}

/* Keeps a run going while held, and returns its length in whole seconds. */
static uint32_t run_s(int64_t *since, bool held, int64_t at_ms)
{
    if (!held) {
        *since = 0;
        return 0;
    }
    if (!*since) {
        *since = at_ms - SAMPLE_MS;
    }
    return (uint32_t)((at_ms - *since) / 1000);
}

static void alarm(state_t *s, int bit, bool on)
{
    uint32_t mask = 1u << bit;
    if (!!(s->alarms & mask) == on) {
        return;
    }
    s->alarms ^= mask;
    journal_post(CRITICAL, "alarm", "{\"alarm\":\"%s\",\"on\":%d,\"temp_c\":%.2f,\"battery\":%.1f}",
                 alarm_names[bit], on, s->now.temp_c, s->now.battery * 100);
}

/* Least squares over the last minute. Returns false without a clear rise. */
static bool rise_rate(float *per_s)
{
    if (ctl.recent_count < HISTORY_S) {
        return false;
    }
    const float mid = (HISTORY_S - 1) / 2.0f;
    float num = 0, den = 0;
    for (int i = 0; i < HISTORY_S; i++) {
        num += (i - mid) * ctl.recent[i];
        den += (i - mid) * (i - mid);
    }
    *per_s = num / den;
    /* Hysteresis, so a slow rise does not toggle the estimate. */
    ctl.rise_known = *per_s * HISTORY_S >= (ctl.rise_known ? MIN_RISE_C / 2 : MIN_RISE_C);
    return ctl.rise_known;
}

static int32_t safe_time(const state_t *s, bool inside)
{
    if (!s->now.temp_valid) {
        return -1;
    }
    if (!inside) {
        return 0;
    }
    float left = INFINITY, rate;
    if (rise_rate(&rate)) {
        left = (SAFE_HIGH_C - s->now.temp_c) / rate;
    }
    if (self == CARRIER && !s->now.door_open) {
        float hold = CARRIER_TAU_S *
                     logf((CARRIER_AMBIENT_C - s->now.temp_c) / (CARRIER_AMBIENT_C - SAFE_HIGH_C));
        left = fminf(left, hold);
    }
    if (isinf(left)) {
        return -1;
    }
    if (!s->now.mains) {
        if (!s->now.battery_valid) {
            return -1;
        }
        left = fminf(left, s->now.battery * BATTERY_FULL_S);
    }
    return (int32_t)left;
}

static void excursions(state_t *s, bool outside)
{
    if (outside && !s->excursion) {
        s->excursion = true;
        s->excursion_count++;
        ctl.excursion_low = ctl.excursion_high = s->now.temp_c;
        ctl.excursion_since = s->now.at_ms;
        journal_post(CRITICAL, "excursion", "{\"on\":1,\"lock\":%lu,\"temp_c\":%.2f}",
                     (unsigned long)s->excursion_count, s->now.temp_c);
        evidence();
    } else if (s->excursion) {
        /* Restored after a reboot without its history. */
        if (ctl.excursion_since == 0) {
            ctl.excursion_low = ctl.excursion_high = s->now.temp_c;
            ctl.excursion_since = s->now.at_ms;
        }
        ctl.excursion_low = fminf(ctl.excursion_low, s->now.temp_c);
        ctl.excursion_high = fmaxf(ctl.excursion_high, s->now.temp_c);
        if (!outside) {
            s->excursion = false;
            journal_post(CRITICAL, "excursion",
                         "{\"on\":0,\"lock\":%lu,\"min_c\":%.2f,\"max_c\":%.2f,\"secs\":%lld}",
                         (unsigned long)s->excursion_count, ctl.excursion_low, ctl.excursion_high,
                         (long long)(s->now.at_ms - ctl.excursion_since) / 1000);
            ctl.excursion_since = 0;
        }
    }
}

/* Detects a compressor that runs without cooling the probe. */
static bool cooling_failed(const state_t *s, bool failed)
{
    const sample_t *n = &s->now;
    bool running = s->relay && n->mains && !n->door_open && n->temp_valid;
    if (running && !ctl.cooling_since) {
        ctl.cooling_start_c = n->temp_c;
    }
    uint32_t secs = run_s(&ctl.cooling_since, running, n->at_ms);
    if (!running) {
        return false;
    }
    if (secs <= COOLING_WINDOW_S) {
        return failed;
    }
    ctl.cooling_since = 0;
    return ctl.cooling_start_c - n->temp_c < COOLING_DROP_C;
}

static void minute_summary(const state_t *s)
{
    const sample_t *n = &s->now;
    if (n->temp_valid) {
        int c = ctl.minute_count++;
        ctl.minute_low = c ? fminf(ctl.minute_low, n->temp_c) : n->temp_c;
        ctl.minute_high = c ? fmaxf(ctl.minute_high, n->temp_c) : n->temp_c;
        ctl.minute_sum = c ? ctl.minute_sum + n->temp_c : n->temp_c;
    }
    if (run_s(&ctl.minute_since, true, n->at_ms) < 60) {
        return;
    }
    if (ctl.minute_count) {
        journal_post(SUMMARY, "summary",
                     "{\"n\":%d,\"min_c\":%.2f,\"max_c\":%.2f,\"avg_c\":%.2f,\"battery\":%.1f}",
                     ctl.minute_count, ctl.minute_low, ctl.minute_high,
                     ctl.minute_sum / ctl.minute_count, n->battery * 100);
    }
    ctl.minute_count = 0;
    ctl.minute_since = 0;
}

/* Updates state and alarms from the new sample. Does no I/O beyond queued records. */
static void evaluate(state_t *s)
{
    const sample_t *n = &s->now;
    /* Nothing is known about the silence, so runs that must be unbroken restart. */
    if (ctl.last_ms && n->at_ms - ctl.last_ms > GAP_MS) {
        ctl.recent_count = 0;
        ctl.rise_known = false;
        ctl.stable_since = 0;
        ctl.cooling_since = 0;
    }
    ctl.last_ms = n->at_ms;
    bool storage = self != INVENTORY;
    if (storage) {
        bool outside = n->temp_valid && (n->temp_c < SAFE_LOW_C || n->temp_c > SAFE_HIGH_C);
        bool inside = n->temp_valid && !outside;
        bool frozen = n->temp_valid && n->temp_c <= FREEZE_C;
        if (n->temp_valid) {
            memmove(ctl.recent, ctl.recent + 1, sizeof ctl.recent - sizeof ctl.recent[0]);
            ctl.recent[HISTORY_S - 1] = n->temp_c;
            ctl.recent_count += ctl.recent_count < HISTORY_S;
        } else {
            ctl.recent_count = 0;
            ctl.rise_known = false;
        }
        s->stable_s = run_s(&ctl.stable_since, inside, n->at_ms);
        if (self == FRIDGE) {
            if (!n->temp_valid || frozen || n->temp_c <= COOL_OFF_C) {
                s->relay = false;
            } else if (n->temp_c >= COOL_ON_C) {
                s->relay = true;
            }
            alarm(s, ALARM_COOLING, cooling_failed(s, s->alarms & 1u << ALARM_COOLING));
        }
        /* An open door stays open across a gap, the cautious reading. */
        s->door_s = run_s(&ctl.door_since, n->door_open, n->at_ms);
        alarm(s, ALARM_DOOR, s->door_s > DOOR_LIMIT_S);
        if (n->temp_valid) {
            excursions(s, outside);
            alarm(s, ALARM_EXCURSION, outside);
            alarm(s, ALARM_FREEZE, frozen);
        }
        /* The first readings precede the first conversion. */
        alarm(s, ALARM_SENSOR, run_s(&ctl.invalid_since, !n->temp_valid, n->at_ms) >= 3);

        bool stored_energy = !n->mains;
        s->safe_s = stored_energy || outside ? safe_time(s, inside) : -1;
        if (self == FRIDGE) {
            alarm(s, ALARM_TRANSPORT,
                  (stored_energy && s->safe_s < 0) || (s->safe_s >= 0 && s->safe_s <= TRANSPORT_S));
        }
        minute_summary(s);
    }
    if (self != CARRIER) {
        alarm(s, ALARM_MAINS, !n->mains);
    }
    if (n->battery_valid) {
        alarm(s, ALARM_BATTERY_LOW, n->battery <= BATTERY_LOW);
        alarm(s, ALARM_BATTERY_CRITICAL, n->battery <= 0.1f);
    }
    alarm(s, ALARM_LOG, journal_low());
}

/* Keeps the excursion across reboots. Retried with the next sample on failure. */
static void persist(uint32_t count, bool excursion)
{
    if (count != ctl.saved_count && save("lock", count)) {
        ctl.saved_count = count;
    }
    if (excursion != ctl.saved_excursion && save("excursion", excursion)) {
        ctl.saved_excursion = excursion;
    }
}

static void controller(void *arg)
{
    esp_task_wdt_add(NULL);
    for (;;) {
        sample_t sample;
        if (xQueueReceive(samples, &sample, pdMS_TO_TICKS(3000)) == pdTRUE) {
            xSemaphoreTake(state_mutex, portMAX_DELAY);
            state.now = sample;
            evaluate(&state);
            bool relay = state.relay;
            bool buzz = state.alarms != 0;
            uint32_t count = state.excursion_count;
            bool excursion = state.excursion;
            xSemaphoreGive(state_mutex);
            bus_relay(relay);
            bus_buzzer(buzz);
            persist(count, excursion);
        }
        esp_task_wdt_reset();
    }
}

void control_start(void)
{
    MUST(state_mutex = xSemaphoreCreateMutex());
    MUST(samples = xQueueCreate(1, sizeof(sample_t)));
    state.excursion_count = ctl.saved_count = restore("lock");
    state.excursion = ctl.saved_excursion = restore("excursion");
    state.transit = restore("transit");
    state.safe_s = -1;
    MUST(xTaskCreate(controller, "control", 6144, NULL, 10, NULL) == pdPASS);
    MUST(xTaskCreate(sampler, "sample", 4096, NULL, 8, NULL) == pdPASS);
}
