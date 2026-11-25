# ColdGuard

小型診所疫苗冷鏈的容錯嵌入式系統。三個 ESP32 節點（冰箱、冷藏箱、庫存終端）執行 FreeRTOS 韌體，透過 MQTT 協作，並在模擬的物理環境中運行。

- 報告日：2026-12-16
- 狀態：技術驗證（spike）尚未開始

## 技術選型

| 項目 | 選擇 |
|---|---|
| 開發與執行環境 | Linux（Ubuntu 24.04） |
| 韌體 | ESP-IDF、C、FreeRTOS |
| 目標晶片 | ESP32-S3（效能不足時改用 ESP32-C3） |
| 晶片模擬 | [esp-emu](https://github.com/espressif/esp-emulator) 1.2.0 |
| 物理模擬與協調 | Rust |
| 3D 呈現 | Rust、Bevy |
| 訊息代理 | Mosquitto |

## 文件

- [架構](docs/architecture.md)：責任邊界、時間同步、介面規則
- [開發計畫](docs/plan.md)：模擬基底、驗收項目、設定與依據
