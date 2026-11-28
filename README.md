# ColdGuard

小型診所疫苗冷鏈的容錯嵌入式系統。三個 ESP32 節點（冰箱、冷藏箱、庫存終端）執行 FreeRTOS 韌體，透過 MQTT 協作，並在模擬的物理環境中運行。

- 報告日：2026-12-16
- 狀態：韌體閉環與劇本重播可執行，驗收結果以 `accept` 為準

## 技術選型

| 項目 | 選擇 |
|---|---|
| 開發與執行環境 | Linux x86_64：Ubuntu 22.04、24.04、26.04，Debian 12、13（CI 逐一驗證）；WSL 以 dozen 使用 GPU |
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

在乾淨容器重現單一發行版的 CI（安裝 apt 套件、rustup、uv，再跑 setup 與測試，驗收不在 CI 內）：

```sh
git worktree add ../coldguard-ci   # 容器以 root 寫入 .tools 與 target，用獨立 worktree 避免污染工作目錄
docker run --rm -v "$PWD/../coldguard-ci:/work" -w /work debian:12 scripts/ci
```

支援的發行版列於 `toolchain.toml` 的 `host.os`，CI 依此清單建立矩陣。WSL 上 `setup` 會另外建置 Mesa 的 dozen（Vulkan on D3D12），讓 viewer 與 `video` 使用 GPU 而非 llvmpipe。

使用 mise 的人也直接執行 `cargo run`：專案的 `mise.toml` 會讓 mise 採用 `rust-toolchain.toml` 的版本。

## 模擬

```sh
cargo run -- demo                      # 3D 診所與三個韌體節點（加 --no-firmware 只跑物理）；點擊門、箱蓋、員工證（或按 1、2、3），點批次盒直接操作該批次，點終端螢幕或按 Tab 看全部庫存與記錄；4 切換 relay，5–8 注入故障，V 換視角，右鍵拖曳與滾輪控制鏡頭
cargo run -- demo scenarios/story.toml # 有字幕與運鏡的主線重播；分支為 excursion、failures、refusals
cargo run -- video film.mp4 scenarios/film.toml scenarios/film-excursion.toml # 約 2 分半的展示片：各劇本先以 1× 跑韌體並檢查，再離線逐幀渲染 1080p60；加 --reuse 只重新渲染
cargo run -- records --latest          # 匯出主機 SQLite 記錄（JSON lines）
cargo run -- accept                    # S、B4、F、D2 驗收，可指定項目，例如 accept F1 F2（S4 需 10 分鐘）
cargo run -- sim scenarios/outage.toml # headless 執行劇本，每行輸出一筆 JSON 快照
cargo test -p sim-core                 # B1、B2 驗收
cargo test                             # B3 虛擬裝置與主機端測試
```

場景模型由 Blender 4.0 產生，修改 `crates/viewer/blender/clinic.py` 後重建 `clinic.glb`：

```sh
cargo run -- assets
```

此命令會依 `uv.lock` 自動同步模型產生器的 Python 依賴，再以 headless Blender 重建模型。

## 文件

- [架構](docs/architecture.md)：責任邊界、時間同步、介面規則
- [開發計畫](docs/plan.md)：模擬基底、驗收項目、設定與依據
