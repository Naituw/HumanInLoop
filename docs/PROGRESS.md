# PROGRESS

记录需要跨会话保留的未完成 / 延期事项和明确下一步。任务 / 需求完成后删除其 section
（历史留在 git）。

## 定期同步：Codex Shell 判定复刻（codex-permission-remember §6.4）

权限记忆功能复刻了 Codex 的 Shell 判定逻辑（`src-tauri/src/shell_safety.rs` +
`permission_shell.rs`）。版本门控只设下限（`VERIFIED_CODEX_VERSION_FLOOR` = **0.122**，
hook 引入版）；`VERIFIED_CODEX_VERSION_CEILING`（当前 **0.146**，对拍来源 Codex upstream
main `1a817bb95d`，2026-07-24；同批吸收 0.145.0 的 #34271 禁选前缀扩容与 #32232
hook-before-guardian 语义，见 spec D50-D53）是最近一次逐行对拍的版本，用户装机超出它时
功能**保持启用**、worker stderr 记一条日志。因此同步不再是紧急事项，但仍需**定期**
（Codex 新 minor 发布后）对拍以下上游文件并抬升已审计版本（相对 codex-rs/）：

- `shell-command/src/bash.rs`（`bash -lc` 脚本拆分）
- `shell-command/src/command_safety/is_safe_command.rs`、`is_dangerous_command.rs`（heuristics）
- `core/src/exec_policy.rs`（fallback 判定 / amendment 派生 / `BANNED_PREFIX_SUGGESTIONS`）
- `config/src/loader/`（配置层叠与项目信任，影响 rules 文件发现与 managed 检测）
- `codex execpolicy check` 的 CLI 契约（参数与 JSON 输出；有 ignored 集成测试
  `permission_shell::tests::real_codex_cli_contract_when_available` 可拿真机验证）

无差异则只改常量 + 记录新 commit；有差异先改 port 再抬已审计版本。若上游出现我方未携带的
**放宽**类变更（新增 safe 命令等），只影响覆盖率；出现语义级破坏（拆分格式、hook 契约）时
fail-closed 机制会自动降级为基础弹窗，届时按 D35 修订的证据链重新评估。

## 待办：Cursor 全局 Rules 迁移为用户级 always-on Skill

调查与候选设计见 `docs/investigations/cursor-global-rule-user-skill.md`。无 workspace folder 的 Cursor IDE
不创建项目 Rules 加载器，因此不会读取 `~/.cursor/rules/askhuman.mdc`。未来改为用户级
`~/.cursor/skills/askhuman/SKILL.md`，旧安装显示“需更新”，迁移时先写新 Skill、再清理旧托管 MDC。
Grok 默认会扫描 Cursor Skills，候选 frontmatter 已设计为对 Cursor 常驻、对 Grok 不可调用。

## 待办：daemon 二进制变化检测 —— 轮询 vs filewatch（后续评估，优先级低）

二进制变化检测目前是 **15s 轮询** `current_exe()` 指纹（稳态≈1 次 `stat`，靠 `binhash.json` 内容哈希缓存避免重哈希）。
是否改 **filewatch** 待权衡——难点：二进制走原子替换（rename 换 inode，需盯父目录 + 按文件名过滤 + 每次替换后重挂，
参考 `config_watch.rs`）、装在任意目录（`~/.local/bin`/brew/npm 前缀/`.app` bundle…）、且 watcher 仍要 stat/hash 才能确认
内容**真**变（指纹是内容哈希而非 mtime）。延迟要求松（~15s 够）+ Hello 路径兜底，故暂保持轮询。
