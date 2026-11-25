/* Append-only flash records, kept until the host confirms them. */

#include "node.h"

#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

#include "esp_partition.h"
#include "esp_rom_crc.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"

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

static const esp_partition_t *part;
static int slots;
static int head;
/* Oldest record that may still be unacknowledged, and the span to head. */
static int tail, span;
static int backlog;
static bool full;
static uint32_t boot, next_seq = 1;
static SemaphoreHandle_t lock;
static QueueHandle_t queue;

static uint32_t checksum(const slot_t *s)
{
    uint32_t crc = esp_rom_crc32_le(0, (const uint8_t *)&s->boot, 8);
    return esp_rom_crc32_le(crc, (const uint8_t *)&s->time_ms,
                            sizeof *s - offsetof(slot_t, time_ms));
}

static bool load(int i, slot_t *s)
{
    return esp_partition_read(part, i * SLOT, s, sizeof *s) == ESP_OK && s->magic == MAGIC
           && s->crc == checksum(s);
}

static void recorder(void *arg)
{
    record_t r;
    for (;;) {
        xQueueReceive(queue, &r, portMAX_DELAY);
        journal_add(r.kind, r.type, "%s", r.body);
    }
}

void journal_init(void)
{
    lock = xSemaphoreCreateMutex();
    queue = xQueueCreate(32, sizeof(record_t));
    part = esp_partition_find_first(ESP_PARTITION_TYPE_DATA, 0x41, "log");
    ESP_ERROR_CHECK(part ? ESP_OK : ESP_ERR_NOT_FOUND);
    slots = part->size / SLOT;

    slot_t s;
    int newest = -1, oldest = -1;
    uint64_t high = 0, low = UINT64_MAX;
    for (int i = 0; i < slots; i++) {
        /* Blank slots are the common case. */
        if (esp_partition_read(part, i * SLOT, &s, 16) != ESP_OK || s.magic != MAGIC || !load(i, &s)) {
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
    xTaskCreate(recorder, "record", 4096, NULL, 4, NULL);
    journal_add(CRITICAL, "boot", "{}");
}

/* Opens the sector at head, if its records are all confirmed. */
static bool open_sector(void)
{
    slot_t s;
    for (int i = head; i < head + PER_SECTOR; i++) {
        if (load(i, &s) && s.unacked) {
            return false;
        }
    }
    return esp_partition_erase_range(part, head * SLOT, SECTOR) == ESP_OK;
}

static bool store(kind_t kind, const char *type, const char *body)
{
    int free = slots - span;
    if (free <= PER_SECTOR || (kind == SUMMARY && free <= RESERVED + PER_SECTOR)) {
        full = kind == CRITICAL || full;
        return false;
    }
    if (head % PER_SECTOR == 0 && !open_sector()) {
        full = true;
        return false;
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
    if (err != ESP_OK) {
        return false;
    }
    backlog++;
    full = false;
    return true;
}

bool journal_add(kind_t kind, const char *type, const char *fmt, ...)
{
    char body[BODY_MAX];
    va_list args;
    va_start(args, fmt);
    vsnprintf(body, sizeof body, fmt, args);
    va_end(args);
    xSemaphoreTake(lock, portMAX_DELAY);
    bool ok = store(kind, type, body);
    xSemaphoreGive(lock);
    return ok;
}

void journal_post(kind_t kind, const char *type, const char *fmt, ...)
{
    record_t r = {.kind = kind};
    strlcpy(r.type, type, sizeof r.type);
    va_list args;
    va_start(args, fmt);
    vsnprintf(r.body, sizeof r.body, fmt, args);
    va_end(args);
    if (xQueueSend(queue, &r, 0) != pdTRUE) {
        full = true;
    }
}

bool journal_pending(int n, record_t *out, int *slot)
{
    slot_t s;
    bool found = false;
    xSemaphoreTake(lock, portMAX_DELAY);
    for (int i = 0; i < span; i++) {
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
        if (n-- == 0) {
            *slot = at;
            out->boot = s.boot;
            out->seq = s.seq;
            out->time_ms = s.time_ms;
            out->kind = s.kind;
            memcpy(out->type, s.type, sizeof out->type);
            memcpy(out->body, s.body, sizeof out->body);
            found = true;
            break;
        }
    }
    xSemaphoreGive(lock);
    return found;
}

void journal_ack(uint32_t ack_boot, uint32_t seq)
{
    slot_t s;
    xSemaphoreTake(lock, portMAX_DELAY);
    for (int i = 0; i < span; i++) {
        int at = (tail + i) % slots;
        if (load(at, &s) && s.boot == ack_boot && s.seq == seq) {
            uint8_t done = 0;
            if (s.unacked
                && esp_partition_write(part, at * SLOT + offsetof(slot_t, unacked), &done, 1) == ESP_OK) {
                backlog--;
                full = false;
            }
            break;
        }
    }
    xSemaphoreGive(lock);
}

int journal_backlog(void)
{
    return backlog;
}

bool journal_full(void)
{
    return full;
}

uint32_t journal_boot(void)
{
    return boot;
}

uint32_t journal_seq(void)
{
    xSemaphoreTake(lock, portMAX_DELAY);
    uint32_t seq = next_seq++;
    xSemaphoreGive(lock);
    return seq;
}
