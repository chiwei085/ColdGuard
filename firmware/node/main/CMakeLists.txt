idf_component_register(
    SRCS "main.c" "bus.c" "journal.c" "control.c" "net.c" "app.c"
    PRIV_REQUIRES esp_wifi esp_event esp_netif nvs_flash esp_partition esp_timer
                  esp_driver_i2c esp_driver_gpio esp_driver_uart esp_rom)
