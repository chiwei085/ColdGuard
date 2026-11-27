#include "node.h"

#include <stdio.h>
#include <string.h>

#include "esp_partition.h"
#include "nvs_flash.h"

const char *const node_names[NODES] = {"fridge", "carrier", "inventory"};
node_t self;
char broker_uri[64];

node_t node_find(const char *name)
{
    node_t n = 0;
    while (n < NODES && strcmp(name, node_names[n]) != 0) {
        n++;
    }
    return n;
}

/* The cfg partition holds "name port" as text. */
static bool configure(void)
{
    const esp_partition_t *part = esp_partition_find_first(ESP_PARTITION_TYPE_DATA, 0x40, "cfg");
    char text[64] = "";
    if (!part || esp_partition_read(part, 0, text, sizeof text - 1) != ESP_OK) {
        return false;
    }
    char name[16];
    int port;
    if (sscanf(text, "%15s %d", name, &port) != 2 || port < 0 || port > 65535) {
        return false;
    }
    self = node_find(name);
    if (self == NODES) {
        return false;
    }
    /* Port 0 is a standalone unit. */
    if (port != 0) {
        snprintf(broker_uri, sizeof broker_uri, "mqtt://192.168.4.1:%d", port);
    }
    return true;
}

void app_main(void)
{
    esp_err_t err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        err = nvs_flash_init();
    }
    ESP_ERROR_CHECK(err);
    if (!configure()) {
        puts("COLDGUARD_NO_CONFIG");
        return;
    }
    /* The clock is set before the first record is stamped. */
    bus_init();
    journal_init();
    control_start();
    app_start();
    net_start();
    printf("COLDGUARD_READY %s\n", node_names[self]);
}
