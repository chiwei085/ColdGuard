# 架構

## 1. 元件

```
          Bevy（3D 呈現）
             │ 指令／狀態快照
          sim-core（熱、電、門、人員、故障）
             │
          coordinator + 虛擬裝置
             │ cosim-tcp ×3
          esp-emu ×3：冰箱／冷藏箱／庫存終端 ──MQTT── Mosquitto
```

所有元件都在同一台 Linux 主機上執行。每個時間量子（quantum）的同步只涉及 `sim-core`、coordinator 與 esp-emu；Bevy 是獨立行程，只接收快照並送出指令，不參與時間同步。

## 2. 責任邊界

| 元件 | 負責 | 不負責 |
|---|---|---|
| 韌體 | 控制、濾除雜訊、警報、批次鎖定、通訊、記錄 | 物理變化 |
| 虛擬裝置 | 物理量與 I²C、UART、GPIO 訊號的轉換 | 控制決策 |
| sim-core | 溫度、電力、門、人員、故障狀態的演進 | 控制決策、匯流排格式 |
| coordinator | 發放模擬時間、連接 sim-core 與三個節點 | 物理與控制 |
| Bevy | 顯示狀態、接收操作 | 保存或計算模擬狀態 |

劇本（`scenarios/*.toml`）可包含輸入 `event`、字幕與視角 `caption`，以及針對韌體回報的期望 `check`；`check` 由 headless 驗收判定，不影響執行。離站搬運由 coordinator 在冷藏箱回報已出發後啟動，sim-core 推進箱體位置。

規則：

1. sim-core 不依賴 Bevy，採固定步長，可 headless 執行。
2. 故障只能改變物理狀態、線路或網路，不得改寫韌體變數。故障注入位於模擬器端，韌體不設後門。
3. Glycol 探頭的熱慣性由 sim-core 以一階濾波計算；韌體不重複套用。
4. 韌體與模擬器只共用腳位表、匯流排位址、暫存器表與訊框格式，不提供模擬器專用的韌體 API。因此介面不限制韌體語言；主韌體使用 C。
5. RFID 使用簡化的 UART 或 I²C 讀取器模型，不模擬完整的 MFRC522。

## 3. 時間同步

依據 esp-emu 的 [esp_cosim.proto](https://github.com/espressif/esp-emulator/blob/main/proto/esp_cosim.proto)。

1. coordinator 對每個節點註冊 `TimeClaim`。未發放 `TimeGrant` 時，晶片停止。
2. 每回合發放 1 ms，等三個節點都回報 `TimeWaiting`，再推進 sim-core。
3. 暫停即停止發放時間。
4. 每個節點只用一條連線持鐘並回應裝置，coordinator 在等待 `TimeWaiting` 時同步回覆匯流排請求，輸入與回覆因此有固定順序。
5. esp-emu 在時鐘被接管前會自由執行，因此接管後先以 EN 斷電重啟，再開始計時；重啟後仍留有 1 ms 內的相位差。
6. grant 是時間增量；晶片重置會讓模擬時間歸零，但剩餘 grant 與世界時間延續，因此以 sim-core 的時間為準。

限制與對策：

| 限制 | 對策 |
|---|---|
| [#71](https://github.com/espressif/esp-emulator/issues/71)：持鐘連線收到所有 GPIO 廣播，大 quantum 可能 deadlock | quantum 用 1 ms；單一連線持續讀取 socket，不在等待時停止讀取 |
| I²C/SPI 讀取超過 5 秒（真實時間）未回覆，esp-emu 會中止 | 虛擬裝置必須回覆每個讀取請求；模擬故障時回 NACK，不可不回覆 |
| 網路封包依真實時間到達 | 有網路的場景不保證可重現，以容許範圍驗證 |
| 快轉過多會使 MQTT 逾時失真 | 網路場景限速：F 類驗收約 12.5×，D2 與展示 1×，展示另用加速的熱參數；離線場景不限速 |

可重現性：

- 單節點、不接網路：以相同輸入重跑，事件順序與內容一致，時間差在 1 ms 內。
- 三節點加 MQTT：以時間容許範圍驗證。

## 4. 韌體規則

- 各節點的感測、控制、警報、記錄與通訊分為獨立 task；警報優先權最高，不等待網路。
- 使用 queue、mutex、event group、software timer 與 watchdog。
- 網路中斷時，本地警報與記錄持續運作。

- 節點名稱與 broker 埠寫在 `cfg` 分割區，所有節點使用同一份韌體；埠為 0 時離線執行。
- RFID UID 以 `04` 開頭者為員工證，其餘為批次標籤。批次操作前須掃描該批標籤，入庫時將標籤與批號綁定。
- 批次位置為 fridge、quarantine、carrier、receiver 或 discard；前兩者由冰箱節點擔保，carrier 由冷藏箱節點擔保。接收確認前，冷藏箱記錄須全部送達。
- 記錄寫入 `log` 分割區，主機寫入 SQLite 後才以 application ACK 確認；鎖定序號與庫存存於 NVS。

## 5. 固定參數

系統採用下列固定參數：

| 項目 | 值 |
|---|---|
| 取樣週期 | 1 s |
| 安全溫度 | 2–8 °C |
| 冷卻控制 | 約 5 °C，具遲滯 |
| 凍結警報 | ≤ 0 °C，高優先 |
| 門未關警報 | > 60 s |
| 心跳 | 5 s；連續遺失 3 次標為離線 |
| 訊息 | 含序號與 checksum；關鍵事件需 ACK，未收到則重送 |

## 6. 物理模型

sim-core 步長 1 ms，與 quantum 相同。初始 5 °C、環溫 25 °C，下列為工程預設，可由劇本覆寫：

| 項目 | 冰箱 | 冷藏箱 |
|---|---|---|
| 熱容量 | 40 kJ/K | 10 kJ/K |
| 熱漏（關門／開門） | 1.5／15 W/K | 0.1／2 W/K |
| 冷卻功率 | 150 W | 0（被動） |
| 探頭時間常數 | 60 s | 60 s |

空氣與內容物視為單一熱容：`C·dT/dt = 熱漏·(環溫 − T) − 冷卻功率`。轉運搬運時間 60 s、初始到貨 3 批。各節點電池 10 Wh、負載 1 W、充電 5 W；冷藏箱僅用電池。感測器故障只影響讀值，不改變溫度。
