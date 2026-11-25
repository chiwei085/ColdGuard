# ColdGuard

小型診所疫苗冷鏈的容錯嵌入式系統。三個 ESP32 節點（冰箱、冷藏箱、庫存終端）執行 FreeRTOS 韌體，透過 MQTT 協作，並在模擬的物理環境中運行。

- 報告日：2026-12-16
- 狀態：韌體閉環與劇本重播可執行，驗收結果以 `accept` 為準

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

## 環境

需要 [rustup](https://rustup.rs) 與 [uv](https://docs.astral.sh/uv/)。版本釘選於 `toolchain.toml` 與 `rust-toolchain.toml`，其餘工具安裝在 `.tools/`。

```sh
cargo run -- doctor                # 檢查環境；缺少系統套件時會列出 apt 指令
cargo run -- setup                 # 安裝工具、建置 smoke 韌體，再執行 doctor
cargo run -- exec idf.py --version # 在專案環境中執行指令
```

若 shell 設定了 `RUSTUP_TOOLCHAIN`（例如 mise），它會蓋過釘選版本，請改用 `cargo +1.100.0`。

## 模擬

```sh
cargo run -- demo                      # 3D 診所與三個韌體節點（加 --no-firmware 只跑物理）；點擊門、箱蓋、員工證（或按 1、2、3），點批次盒直接操作該批次，點終端螢幕或按 Tab 看全部庫存與記錄；4 切換 relay，5–8 注入故障，V 換視角，右鍵拖曳與滾輪控制鏡頭
cargo run -- demo scenarios/story.toml # 有字幕與運鏡的主線重播；分支為 excursion、failures、refusals
cargo run -- records --latest          # 匯出主機 SQLite 記錄（JSON lines）
cargo run -- accept                    # S、B4、F、D2 驗收，可指定項目，例如 accept F1 F2（S4 需 10 分鐘）
cargo run -- sim scenarios/outage.toml # headless 執行劇本，每行輸出一筆 JSON 快照
cargo test -p sim-core                 # B1、B2 驗收
cargo test                             # B3 虛擬裝置與主機端測試
```

場景模型由 Blender 4.0 產生，修改 `crates/viewer/blender/clinic.py` 後重建 `clinic.glb`：

```sh
uv pip install --python /usr/bin/python3.12 --target .tools/blender-py numpy==1.26.4
PYTHONPATH=.tools/blender-py blender -b --factory-startup --python-use-system-env -P crates/viewer/blender/clinic.py
```

## 文件

- [架構](docs/architecture.md)：責任邊界、時間同步、介面規則
- [開發計畫](docs/plan.md)：模擬基底、驗收項目、設定與依據
