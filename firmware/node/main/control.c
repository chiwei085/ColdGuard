/* Sampling, cooling control and alarms. Independent of the network. */

#include "node.h"

#include <math.h>
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
#define WINDOW_S 60
/* Below four sensor steps a rise is indistinguishable from noise. */
#define MIN_RISE_C 0.031f
/* Packing, travel and margin. */
#define TRANSPORT_S ((5 + 15 + 15) * 60)
/* Battery capacity over monitoring load. */
#define BATTERY_FULL_S 36000
/* Passive carrier, validated model at high ambient. */
#define CARRIER_TAU_S 100000.0f
#define CARRIER_AMBIENT_C 40.0f

const char *const alarm_names[ALARMS] = {
    "door_open",   "excursion",        "freeze",    "sensor",  "cooling",
    "mains_lost",  "battery_low",      "battery_critical",     "transport", "log_full",
};

static state_t state;
static SemaphoreHandle_t lock;
static QueueHandle_t samples;
static float recent[WINDOW_S];
static int recent_count;

state_t state_get(void)
{
    xSemaphoreTake(lock, portMAX_DELAY);
    state_t copy = state;
    xSemaphoreGive(lock);
    return copy;
}

static void save(const char *key, uint32_t value)
{
    nvs_handle_t h;
    if (nvs_open("node", NVS_READWRITE, &h) == ESP_OK) {
        nvs_set_u32(h, key, value);
        nvs_commit(h);
        nvs_close(h);
    }
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

void state_transit(uint8_t transit, bool receiver_ready)
{
    xSemaphoreTake(lock, portMAX_DELAY);
    state.transit = transit;
    state.receiver_ready = receiver_ready;
    xSemaphoreGive(lock);
    save("transit", transit);
}

void state_person(const char *uid)
{
    xSemaphoreTake(lock, portMAX_DELAY);
    strlcpy(state.person, uid, sizeof state.person);
    state.person_ms = now_ms();
    xSemaphoreGive(lock);
}

int control_recent(float out[60])
{
    xSemaphoreTake(lock, portMAX_DELAY);
    memcpy(out, recent, sizeof recent);
    int n = recent_count;
    xSemaphoreGive(lock);
    return n;
}

static void sampler(void *arg)
{
    esp_task_wdt_add(NULL);
    TickType_t wake = xTaskGetTickCount();
    for (;;) {
        sample_t s;
        bus_sample(&s);
        xQueueOverwrite(samples, &s);
        esp_task_wdt_reset();
        vTaskDelayUntil(&wake, pdMS_TO_TICKS(1000));
    }
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
    if (recent_count < WINDOW_S) {
        return false;
    }
    const float mid = (WINDOW_S - 1) / 2.0f;
    float num = 0, den = 0;
    for (int i = 0; i < WINDOW_S; i++) {
        num += (i - mid) * recent[i];
        den += (i - mid) * (i - mid);
    }
    *per_s = num / den;
    /* Hysteresis, so a slow rise does not toggle the estimate. */
    static bool known;
    known = *per_s * WINDOW_S >= (known ? MIN_RISE_C / 2 : MIN_RISE_C);
    return known;
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
        float hold = CARRIER_TAU_S
                     * logf((CARRIER_AMBIENT_C - s->now.temp_c) / (CARRIER_AMBIENT_C - SAFE_HIGH_C));
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
    static float low, high;
    static int64_t since;
    if (outside && !s->excursion) {
        s->excursion = true;
        s->lock++;
        low = high = s->now.temp_c;
        since = now_ms();
        save("lock", s->lock);
        save("excursion", 1);
        journal_post(CRITICAL, "excursion", "{\"on\":1,\"lock\":%lu,\"temp_c\":%.2f}",
                     (unsigned long)s->lock, s->now.temp_c);
        net_samples();
    } else if (s->excursion) {
        /* Restored after a reboot without its history. */
        if (since == 0) {
            low = high = s->now.temp_c;
            since = now_ms();
        }
        low = fminf(low, s->now.temp_c);
        high = fmaxf(high, s->now.temp_c);
        if (!outside) {
            s->excursion = false;
            save("excursion", 0);
            journal_post(CRITICAL, "excursion",
                         "{\"on\":0,\"lock\":%lu,\"min_c\":%.2f,\"max_c\":%.2f,\"secs\":%lld}",
                         (unsigned long)s->lock, low, high, (long long)(now_ms() - since) / 1000);
        }
    }
}

/* Detects a compressor that runs without cooling the probe. */
static bool cooling_failed(const state_t *s, bool failed)
{
    static int seconds;
    static float start;
    const sample_t *n = &s->now;
    if (!s->relay || !n->mains || n->door_open || !n->temp_valid) {
        seconds = 0;
        return false;
    }
    if (seconds++ == 0) {
        start = n->temp_c;
    }
    if (seconds <= COOLING_WINDOW_S) {
        return failed;
    }
    seconds = 0;
    return start - n->temp_c < COOLING_DROP_C;
}

static void minute_summary(const state_t *s)
{
    static int count, total;
    static float low, high, sum;
    const sample_t *n = &s->now;
    if (n->temp_valid) {
        low = count ? fminf(low, n->temp_c) : n->temp_c;
        high = count ? fmaxf(high, n->temp_c) : n->temp_c;
        sum = count ? sum + n->temp_c : n->temp_c;
        count++;
    }
    if (++total < 60) {
        return;
    }
    if (count) {
        journal_post(SUMMARY, "summary",
                     "{\"n\":%d,\"min_c\":%.2f,\"max_c\":%.2f,\"avg_c\":%.2f,\"battery\":%.1f}", count,
                     low, high, sum / count, n->battery * 100);
    }
    count = total = 0;
}

static void evaluate(state_t *s)
{
    static int door_s;
    const sample_t *n = &s->now;
    bool storage = self != INVENTORY;
    if (storage) {
        bool outside = n->temp_valid && (n->temp_c < SAFE_LOW_C || n->temp_c > SAFE_HIGH_C);
        bool inside = n->temp_valid && !outside;
        bool frozen = n->temp_valid && n->temp_c <= FREEZE_C;
        if (n->temp_valid) {
            memmove(recent, recent + 1, sizeof recent - sizeof recent[0]);
            recent[WINDOW_S - 1] = n->temp_c;
            recent_count += recent_count < WINDOW_S;
        } else {
            recent_count = 0;
        }
        s->stable_s = inside ? s->stable_s + 1 : 0;
        if (self == FRIDGE) {
            if (!n->temp_valid || frozen || n->temp_c <= COOL_OFF_C) {
                s->relay = false;
            } else if (n->temp_c >= COOL_ON_C) {
                s->relay = true;
            }
            alarm(s, ALARM_COOLING, cooling_failed(s, s->alarms & 1u << ALARM_COOLING));
        }
        door_s = n->door_open ? door_s + 1 : 0;
        alarm(s, ALARM_DOOR, door_s > DOOR_LIMIT_S);
        if (n->temp_valid) {
            excursions(s, outside);
            alarm(s, ALARM_EXCURSION, outside);
            alarm(s, ALARM_FREEZE, frozen);
        }
        /* The first readings precede the first conversion. */
        static int invalid_s;
        invalid_s = n->temp_valid ? 0 : invalid_s + 1;
        alarm(s, ALARM_SENSOR, invalid_s >= 3);

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
        alarm(s, ALARM_BATTERY_LOW, n->battery <= 0.2f);
        alarm(s, ALARM_BATTERY_CRITICAL, n->battery <= 0.1f);
    }
    alarm(s, ALARM_LOG, journal_full());
}

static void controller(void *arg)
{
    esp_task_wdt_add(NULL);
    for (;;) {
        sample_t sample;
        if (xQueueReceive(samples, &sample, pdMS_TO_TICKS(3000)) == pdTRUE) {
            xSemaphoreTake(lock, portMAX_DELAY);
            state.now = sample;
            evaluate(&state);
            bool relay = state.relay;
            bool buzz = state.alarms != 0;
            xSemaphoreGive(lock);
            bus_relay(relay);
            bus_buzzer(buzz);
        }
        esp_task_wdt_reset();
    }
}

void control_start(void)
{
    lock = xSemaphoreCreateMutex();
    samples = xQueueCreate(1, sizeof(sample_t));
    state.lock = restore("lock");
    state.excursion = restore("excursion");
    state.transit = restore("transit");
    state.safe_s = -1;
    xTaskCreate(controller, "control", 6144, NULL, 10, NULL);
    xTaskCreate(sampler, "sample", 4096, NULL, 8, NULL);
}
