# 開發規則

- 驗收與版本不綁定：一個版本可涵蓋任意多個驗收項目，一個項目也可跨版本完成。
- B、F 類驗收項目須能 headless 執行。
- `docs/` 只寫系統設計、設定與驗收條件；開發流程與協作規則寫在本檔。

## Commit

採用 [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/)：`<type>(<scope>): <description>`，scope 可省略。

- type：`feat`、`fix`、`docs`、`refactor`、`perf`、`test`、`build`、`ci`、`chore`、`revert`。
- 不相容變更在冒號前加 `!`，或在 footer 寫 `BREAKING CHANGE: <說明>`。
