# Codex Compaction 展示发布说明

升级后，Usagi 会通过现有 shadow rebuild 重新扫描历史 Codex usage，并在新代次满足切换条件后启用分类结果。重建过程中，尚未 ready 的模型块显示 `—`；不会直接改写 active 历史账目。

Codex Main 与 Subagent 的模型用量块会在 `Estimated Cost` 上方显示 `Compaction`。该值是已计入 canonical usage 的 Compaction response 总量子集。`Total Tokens` 已包含这部分实际用量；Compaction 行只说明其分类，不会再次增加 tokens 或 cost。

`—` 表示当前分类仍未知，例如 marker 尚未解析或扫描尚未 ready。扫描范围已完成且没有 Compaction marker 时显示 `0`。已解析 marker 会按 owning thread、model 和 effort 归入对应块；其他 provider 不显示该行。

升级验证中的历史 header 是测试构造：它将 v6 payload 放入 parser v11 / canonical algorithm v5 metadata header 后执行重建，不代表真实 v5 旧账本或真实 v5 usage 账目。
