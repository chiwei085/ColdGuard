/* Toolchain smoke test. coldguard doctor matches the COLDGUARD_ markers. */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "esp_event.h"
#include "esp_netif.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "mqtt_client.h"
#include "nvs_flash.h"

#define TOPIC "coldguard/smoke"

static char broker_uri[48];

static void on_mqtt(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    esp_mqtt_event_handle_t event = data;
    switch (id) {
    case MQTT_EVENT_CONNECTED:
        esp_mqtt_client_subscribe(event->client, TOPIC, 1);
        break;
    case MQTT_EVENT_SUBSCRIBED:
        esp_mqtt_client_publish(event->client, TOPIC, "ping", 0, 1, 0);
        break;
    case MQTT_EVENT_DATA:
        if (event->data_len == 4 && memcmp(event->data, "ping", 4) == 0) {
            puts("COLDGUARD_MQTT_OK");
        }
        break;
    default:
        break;
    }
}

static void on_ip(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    esp_mqtt_client_config_t config = {.broker.address.uri = broker_uri};
    esp_mqtt_client_handle_t client = esp_mqtt_client_init(&config);
    ESP_ERROR_CHECK(esp_mqtt_client_register_event(client, MQTT_EVENT_ANY, on_mqtt, NULL));
    ESP_ERROR_CHECK(esp_mqtt_client_start(client));
}

/* Use the default access point of esp-emu. */
static void start_wifi(void)
{
    ESP_ERROR_CHECK(nvs_flash_init());
    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    esp_netif_create_default_wifi_sta();
    wifi_init_config_t init = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&init));
    ESP_ERROR_CHECK(esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, on_ip, NULL));
    wifi_config_t sta = {.sta = {.ssid = "myssid", .password = "mypassword"}};
    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &sta));
    ESP_ERROR_CHECK(esp_wifi_start());
    ESP_ERROR_CHECK(esp_wifi_connect());
}

void app_main(void)
{
    puts("COLDGUARD_BOOT_OK");

    /* Without a broker port the firmware stays offline. */
    char line[8] = "";
    size_t length = 0;
    while (length + 1 < sizeof line) {
        int c = getchar();
        if (c == EOF) {
            vTaskDelay(pdMS_TO_TICKS(10));
        } else if (c == '\n' || c == '\r') {
            break;
        } else {
            line[length++] = (char)c;
        }
    }
    int port = atoi(line);
    if (port <= 0 || port > 65535) {
        puts("COLDGUARD_BAD_PORT");
        return;
    }
    /* esp-emu maps its gateway to host loopback. */
    snprintf(broker_uri, sizeof broker_uri, "mqtt://192.168.4.1:%d", port);
    start_wifi();
}
