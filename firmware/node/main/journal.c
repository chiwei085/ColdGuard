/* Append-only flash records, kept until the host confirms them. */

#include "node.h"

#include <stdarg.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

#include "esp_partition.h"
#include "esp_rom_crc.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"

/* Identifies the slot layout. Change it with slot_t, so old slots read as blank. */
#define MAGIC 0xC01D
#define SECTOR 4096
/* Kept free for critical records. */
#define RESERVED 1024

typedef struct {
    uint16_t magic;
    uint8_t kind;
    /* Programmed from 0xFF to 0 after the fact, so it is not in the CRC. */
    uint8_t unacked;
    uint32_t boot;
    uint32_t seq;
    uint32_t crc;
    int64_t time_ms;
    char type[12];
    char body[BODY_MAX];
} slot_t;

_Static_assert(sizeof(slot_t) == 256 || sizeof(slot_t) == 252, "slot layout");

#define SLOT 256
#define PER_SECTOR (SECTOR / SLOT)

/* Set before any task starts. */
static const esp_partition_t *part;
static int slots;
static uint32_t boot;

/* Guarded by journal_mutex. */
static int head;
/* Oldest record that may still be unacknowledged, and the span to head. */
static int tail, span;
static uint32_t next_seq = 1;

/* Written under journal_mutex, read by any task. */
static atomic_int backlog;
static atomic_bool no_room, scarce, fault;
/* Critical records dropped since the last "lost" record. Any task adds to it. */
static atomic_uint lost;

static SemaphoreHandle_t journal_mutex;
static QueueHandle_t queue;

static uint32_t checksum(const slot_t *s)
{
    uint32_t crc = esp_rom_crc32_le(0, (const uint8_t *)&s->boot, 8);
    return esp_rom_crc32_le(crc, (const uint8_t *)&s->time_ms,
                            sizeof *s - offsetof(slot_t, time_ms));
}

static bool load(int i, slot_t *s)
{
    return esp_partition_read(part, i * SLOT, s, sizeof *s) == ESP_OK && s->magic == MAGIC &&
           s->crc == checksum(s);
}

static void update_capacity(void)
{
    atomic_store(&no_room, slots - span <= PER_SECTOR);
    atomic_store(&scarce, slots - span <= RESERVED + PER_SECTOR);
}

static bool store(kind_t kind, const char *type, const char *body);

/* Records the loss once there is room, so a later success cannot hide it. */
static void record_loss(void)
{
    unsigned n = atomic_load(&lost);
    char body[24];
    snprintf(body, sizeof body, "{\"n\":%u}", n);
    if (n && store(CRITICAL, "lost", body)) {
        atomic_fetch_sub(&lost, n);
    }
}

static void recorder(void *arg)
{
    record_t r;
    for (;;) {
        if (xQueueReceive(queue, &r, pdMS_TO_TICKS(1000)) == pdTRUE) {
            journal_add(r.kind, r.type, "%s", r.body);
        } else if (atomic_load(&lost)) {
            xSemaphoreTake(journal_mutex, portMAX_DELAY);
            record_loss();
            xSemaphoreGive(journal_mutex);
        }
    }
}

void journal_init(void)
{
    MUST(journal_mutex = xSemaphoreCreateMutex());
    MUST(queue = xQueueCreate(32, sizeof(record_t)));
    part = esp_partition_find_first(ESP_PARTITION_TYPE_DATA, 0x41, "log");
    ESP_ERROR_CHECK(part ? ESP_OK : ESP_ERR_NOT_FOUND);
    slots = part->size / SLOT;

    slot_t s;
    int newest = -1, oldest = -1;
    uint64_t high = 0, low = UINT64_MAX;
    for (int i = 0; i < slots; i++) {
        /* Blank slots are the common case. */
        if (esp_partition_read(part, i * SLOT, &s, 16) != ESP_OK || s.magic != MAGIC ||
            !load(i, &s)) {
            continue;
        }
        uint64_t order = (uint64_t)s.boot << 32 | s.seq;
        if (newest < 0 || order >= high) {
            high = order;
            newest = i;
        }
        if (s.unacked) {
            backlog++;
            if (order < low) {
                low = order;
                oldest = i;
            }
        }
    }
    boot = (uint32_t)(high >> 32) + 1;
    /* A torn write may follow the newest record, so start on a fresh sector. */
    head = newest < 0 ? 0 : (newest / PER_SECTOR + 1) * PER_SECTOR % slots;
    tail = oldest < 0 ? head : oldest;
    span = (head - tail + slots) % slots;
    if (oldest >= 0 && span == 0) {
        span = slots;
    }
    update_capacity();
    MUST(xTaskCreate(recorder, "record", 4096, NULL, 4, NULL) == pdPASS);
    journal_add(CRITICAL, "boot", "{}");
}

static bool sector_confirmed(void)
{
    slot_t s;
    for (int i = head; i < head + PER_SECTOR; i++) {
        if (load(i, &s) && s.unacked) {
            return false;
        }
    }
    return true;
}

static bool store(kind_t kind, const char *type, const char *body)
{
    if (atomic_load(&no_room) || (kind == SUMMARY && atomic_load(&scarce))) {
        return false;
    }
    if (head % PER_SECTOR == 0) {
        if (!sector_confirmed()) {
            atomic_store(&no_room, true);
            return false;
        }
        if (esp_partition_erase_range(part, head * SLOT, SECTOR) != ESP_OK) {
            atomic_store(&fault, true);
            return false;
        }
    }
    slot_t s;
    memset(&s, 0, sizeof s);
    s.magic = MAGIC;
    s.kind = kind;
    s.unacked = 0xff;
    s.boot = boot;
    s.seq = next_seq++;
    s.time_ms = now_ms();
    strlcpy(s.type, type, sizeof s.type);
    strlcpy(s.body, body, sizeof s.body);
    s.crc = checksum(&s);
    esp_err_t err = esp_partition_write(part, head * SLOT, &s, sizeof s);
    head = (head + 1) % slots;
    span++;
    update_capacity();
    atomic_store(&fault, err != ESP_OK);
    if (err != ESP_OK) {
        return false;
    }
    atomic_fetch_add(&backlog, 1);
    return true;
}

bool journal_add(kind_t kind, const char *type, const char *fmt, ...)
{
    char body[BODY_MAX];
    va_list args;
    va_start(args, fmt);
    /* A truncated body is not valid JSON. */
    bool fits = vsnprintf(body, sizeof body, fmt, args) < (int)sizeof body;
    va_end(args);
    xSemaphoreTake(journal_mutex, portMAX_DELAY);
    record_loss();
    bool ok = fits && store(kind, type, body);
    xSemaphoreGive(journal_mutex);
    if (!ok && kind == CRITICAL) {
        atomic_fetch_add(&lost, 1);
    }
    return ok;
}

void journal_post(kind_t kind, const char *type, const char *fmt, ...)
{
    record_t r = {.kind = kind};
    strlcpy(r.type, type, sizeof r.type);
    va_list args;
    va_start(args, fmt);
    bool fits = vsnprintf(r.body, sizeof r.body, fmt, args) < (int)sizeof r.body;
    va_end(args);
    if ((!fits || xQueueSend(queue, &r, 0) != pdTRUE) && kind == CRITICAL) {
        atomic_fetch_add(&lost, 1);
    }
}

int journal_pending(record_t *out, int max)
{
    slot_t s;
    int found = 0;
    xSemaphoreTake(journal_mutex, portMAX_DELAY);
    for (int i = 0; i < span && found < max; i++) {
        int at = (tail + i) % slots;
        if (!load(at, &s) || !s.unacked) {
            /* Drop the confirmed front so later searches stay short. */
            if (i == 0) {
                tail = (tail + 1) % slots;
                span--;
                i--;
            }
            continue;
        }
        record_t *r = &out[found++];
        r->boot = s.boot;
        r->seq = s.seq;
        r->time_ms = s.time_ms;
        r->kind = s.kind;
        memcpy(r->type, s.type, sizeof r->type);
        memcpy(r->body, s.body, sizeof r->body);
    }
    update_capacity();
    xSemaphoreGive(journal_mutex);
    return found;
}

void journal_ack(uint32_t ack_boot, uint32_t seq)
{
    slot_t s;
    xSemaphoreTake(journal_mutex, portMAX_DELAY);
    for (int i = 0; i < span; i++) {
        int at = (tail + i) % slots;
        if (!load(at, &s)) {
            continue;
        }
        /* Records are in order. A repeated ack must not scan the whole log. */
        if (s.boot > ack_boot || (s.boot == ack_boot && s.seq > seq)) {
            break;
        }
        if (s.boot == ack_boot && s.seq == seq) {
            uint8_t done = 0;
            if (s.unacked && esp_partition_write(part, at * SLOT + offsetof(slot_t, unacked), &done,
                                                 1) == ESP_OK) {
                atomic_fetch_sub(&backlog, 1);
            }
            break;
        }
    }
    xSemaphoreGive(journal_mutex);
}

int journal_backlog(void)
{
    return atomic_load(&backlog);
}

bool journal_full(void)
{
    return atomic_load(&no_room) || atomic_load(&fault);
}

bool journal_low(void)
{
    return journal_full() || atomic_load(&scarce) || atomic_load(&lost);
}

uint32_t journal_boot(void)
{
    return boot;
}

uint32_t journal_seq(void)
{
    xSemaphoreTake(journal_mutex, portMAX_DELAY);
    uint32_t seq = next_seq++;
    xSemaphoreGive(journal_mutex);
    return seq;
}
