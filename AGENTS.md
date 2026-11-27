# 開發規則

- 驗收與版本不綁定：一個版本可涵蓋任意多個驗收項目，一個項目也可跨版本完成。
- B、F 類驗收項目須能 headless 執行。
- `docs/` 只寫系統設計、設定與驗收條件。開發流程與協作規則寫在本檔。

## C 開發工具

- 格式採根目錄的 `.clang-format`，使用 clang-format 18 以上。格式化：`clang-format -i firmware/node/main/*.[ch] firmware/smoke/main/*.c`。檢查：`clang-format --dry-run --Werror firmware/node/main/*.[ch] firmware/smoke/main/*.c`。
- clangd 為可選的本機輔助。先完成 `cargo run -- setup`。VS Code 以專案根目錄開啟工作區並啟用建議的 clangd 擴充套件，`.vscode/settings.json` 已設定啟動腳本、C 標頭辨識與 C 存檔格式化，並關閉 C/C++ 擴充套件的 IntelliSense 以避免重複診斷。其他編輯器將 clangd 執行檔設為本專案 `scripts/clangd` 的絕對路徑。不使用 clangd 時停用該擴充套件即可，命令列格式化與韌體建置不依賴它。
- `scripts/clangd` 透過既有專案環境啟動 ESP-IDF 釘選的 Espressif clangd，並只允許查詢本專案的 ESP32-S3 GCC。勿使用系統 clangd 代替。
- `.clangd` 讀取 `.tools/node/compile_commands.json` 與 `.tools/smoke/compile_commands.json`。變更元件或 SDK 設定後，以既有建置命令重新產生，不手改編譯資料庫。
- Node 重新建置：`cargo run -- exec idf.py -C firmware/node -B .tools/node -D SDKCONFIG="$PWD/.tools/node/sdkconfig" -D IDF_TARGET=esp32s3 build`。
- Headless 檢查範例：`scripts/clangd --check=firmware/node/main/main.c`。clangd 解析專用的參數過濾只放在 `.clangd`，不得加入 CMake、sdkconfig 或韌體原始碼。工具不加入韌體相依套件或燒錄映像，快取留在已忽略的 `.cache/` 或 `.tools/`。
- 批次只檢查語法時加 `--tweaks=`。釘選版本的 `SwapBinaryOperands` 重構自測會在部分 FreeRTOS 巨集上報重疊替換錯誤，與語法診斷無關。編輯器啟動時不需停用重構功能。

## 註釋

- 所有程式碼與設定檔註釋（含文件註釋）一律使用最簡潔且清楚的英文。
- 註釋禁用分號（`;`、`；`）與破折號（`—`、`–`、`--`），改用句號或改寫句子。
- 註釋說明用途與原因，不逐步解釋程式如何運作，也不重述程式碼或函式簽名已表達的資訊。
- 優先用清楚的命名與簡單的結構表達意圖，不用註釋補救難讀的程式碼。需要大量註釋時，先重構。
- 函式用途與必要背景寫在函式前。函式內只保留非直觀限制、陷阱或特殊處理原因的短註釋，避免過度註釋。
- 資料註釋只補充名稱與型別無法表達的用途、單位或限制。API 文件註釋使用該語言的標準格式，省略無資訊的樣板。
- 修改程式時同步更新或刪除失效、重複與多餘的註釋。

## Commit

採用 [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/)：`<type>(<scope>): <description>`，scope 可省略。

- type：`feat`、`fix`、`docs`、`refactor`、`perf`、`test`、`build`、`ci`、`chore`、`revert`。
- 不相容變更在冒號前加 `!`，或在 footer 寫 `BREAKING CHANGE: <說明>`。
