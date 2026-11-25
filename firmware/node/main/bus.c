/* Sensor, power, relay and tag reader access. */

#include "node.h"

#include <string.h>

#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/uart.h"

#define PIN_SDA 8
#define PIN_SCL 9
#define PIN_RELAY 4
#define PIN_DOOR 5
#define PIN_MAINS 6
#define PIN_BUZZER 7
#define PIN_RFID_TX 17
#define PIN_RFID_RX 18
#define RFID_UART UART_NUM_1

#define TMP117_ADDR 0x48
#define TMP117_TEMP 0x00
#define TMP117_CONFIG 0x01
#define TMP117_READY 0x2000
/* Continuous, 8 averages, 125 ms cycle. Outruns the 1 s sampling. */
#define TMP117_CONFIG_VALUE 0x00A0
#define TMP117_LSB 0.0078125f
/* Reported until the first conversion. */
#define TMP117_NO_DATA ((int16_t)0x8000)

#define BATTERY_ADDR 0x36
#define BATTERY_LEVEL 0x00
#define BATTERY_MV 0x02

#define I2C_TIMEOUT_MS 50

static i2c_master_dev_handle_t thermometer, battery;
static bool configured;

static i2c_master_dev_handle_t attach(i2c_master_bus_handle_t bus, uint16_t address)
{
    i2c_device_config_t config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = address,
        .scl_speed_hz = 100000,
    };
    i2c_master_dev_handle_t device;
    ESP_ERROR_CHECK(i2c_master_bus_add_device(bus, &config, &device));
    return device;
}

void bus_init(void)
{
    gpio_config_t out = {
        .pin_bit_mask = BIT64(PIN_RELAY) | BIT64(PIN_BUZZER),
        .mode = GPIO_MODE_OUTPUT,
    };
    ESP_ERROR_CHECK(gpio_config(&out));
    bus_relay(false);
    bus_buzzer(false);
    gpio_config_t in = {
        .pin_bit_mask = BIT64(PIN_DOOR) | BIT64(PIN_MAINS),
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,
    };
    ESP_ERROR_CHECK(gpio_config(&in));

    i2c_master_bus_config_t i2c = {
        .i2c_port = I2C_NUM_0,
        .sda_io_num = PIN_SDA,
        .scl_io_num = PIN_SCL,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    i2c_master_bus_handle_t bus;
    ESP_ERROR_CHECK(i2c_new_master_bus(&i2c, &bus));
    battery = attach(bus, BATTERY_ADDR);
    if (self != INVENTORY) {
        thermometer = attach(bus, TMP117_ADDR);
    }

    uart_config_t uart = {
        .baud_rate = 9600,
        .data_bits = UART_DATA_8_BITS,
        .parity = UART_PARITY_DISABLE,
        .stop_bits = UART_STOP_BITS_1,
        .flow_ctrl = UART_HW_FLOWCTRL_DISABLE,
    };
    ESP_ERROR_CHECK(uart_driver_install(RFID_UART, 256, 0, 0, NULL, 0));
    ESP_ERROR_CHECK(uart_param_config(RFID_UART, &uart));
    ESP_ERROR_CHECK(uart_set_pin(RFID_UART, PIN_RFID_TX, PIN_RFID_RX, UART_PIN_NO_CHANGE,
                                 UART_PIN_NO_CHANGE));
}

static bool read16(i2c_master_dev_handle_t device, uint8_t reg, uint16_t *value)
{
    uint8_t data[2];
    if (i2c_master_transmit_receive(device, &reg, 1, data, 2, I2C_TIMEOUT_MS) != ESP_OK) {
        return false;
    }
    *value = data[0] << 8 | data[1];
    return true;
}

static bool read_temperature(float *celsius)
{
    uint16_t value;
    if (!configured) {
        uint8_t setup[] = {TMP117_CONFIG, TMP117_CONFIG_VALUE >> 8, TMP117_CONFIG_VALUE & 0xff};
        configured = i2c_master_transmit(thermometer, setup, sizeof setup, I2C_TIMEOUT_MS) == ESP_OK;
        return false;
    }
    if (!read16(thermometer, TMP117_CONFIG, &value)) {
        /* A replaced sensor starts from its defaults. */
        configured = false;
        return false;
    }
    if (!(value & TMP117_READY) || !read16(thermometer, TMP117_TEMP, &value)) {
        return false;
    }
    *celsius = (int16_t)value * TMP117_LSB;
    return (int16_t)value != TMP117_NO_DATA;
}

void bus_sample(sample_t *out)
{
    uint16_t level = 0, mv = 0;
    memset(out, 0, sizeof *out);
    out->temp_valid = thermometer && read_temperature(&out->temp_c);
    out->battery_valid =
        read16(battery, BATTERY_LEVEL, &level) && read16(battery, BATTERY_MV, &mv) && level <= 10000;
    out->battery = level / 10000.0f;
    out->battery_mv = mv;
    out->door_open = thermometer && gpio_get_level(PIN_DOOR);
    out->mains = gpio_get_level(PIN_MAINS);
}

void bus_relay(bool on)
{
    gpio_set_level(PIN_RELAY, on);
}

void bus_buzzer(bool on)
{
    gpio_set_level(PIN_BUZZER, on);
}

static int hex(char c)
{
    if (c >= '0' && c <= '9') {
        return c - '0';
    }
    if (c >= 'A' && c <= 'F') {
        return c - 'A' + 10;
    }
    return -1;
}

/* STX, 8 UID digits, 2 XOR digits, ETX. */
bool bus_tag(char uid[UID_LEN + 1])
{
    uint8_t c;
    do {
        uart_read_bytes(RFID_UART, &c, 1, portMAX_DELAY);
    } while (c != 0x02);
    char frame[11];
    uid[0] = '\0';
    if (uart_read_bytes(RFID_UART, frame, sizeof frame, pdMS_TO_TICKS(200)) != sizeof frame
        || frame[10] != 0x03) {
        return false;
    }
    int sum = 0;
    for (int i = 0; i < 10; i += 2) {
        int high = hex(frame[i]), low = hex(frame[i + 1]);
        if (high < 0 || low < 0) {
            return false;
        }
        sum ^= high << 4 | low;
    }
    if (sum != 0) {
        return false;
    }
    memcpy(uid, frame, UID_LEN);
    uid[UID_LEN] = '\0';
    return true;
}
