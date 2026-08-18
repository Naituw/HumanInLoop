# PROGRESS

记录需要跨会话保留的未完成 / 延期事项和明确下一步。任务 / 需求完成后删除其 section
（历史留在 git）。

## 待外部验收：Windows 发布候选

功能与架构实现已在 `codex/windows-platform-parity` 完成；设计、实施记录和 Win11 证据见
`docs/specs/windows-platform-parity.md`、`docs/plans/windows-platform-parity.md` §17.9 与
`docs/plans/windows-unsigned-update-policy.md` §12。当前 Win11 24H2 VM 已通过 PS5/PS7 install、
named-pipe daemon、完整 Rust tests（1127 passed / 2 ignored）、Windows update 专属测试（19/19）、
Clippy、165 Vitest + 5 Node tests、production/release build、真实 authenticated Codex 0.147 E2E、
卸载维护链和未签名 binary fail-closed / `update prepare` 文件锁闭环。交互式桌面已覆盖统一图标、Ctrl
快捷键、Advanced、设置稳定性、真实飞书取消、Windows Terminal 精确 focus、Dev Instance 以及无闪窗
login/logout。外部平台逻辑 Review 的 correctness 项也已收口：daemon metadata watcher、共享 Windows
path identity、launchId 平台标记和 rename 覆盖语义均有 Windows 原生回归证据。

发布认证仍依赖仓库外状态：

- 准备干净 Windows 10 22H2 x64 VM 并复跑核心矩阵；
- 生产 Authenticode、timestamp、publisher identity pinning 与 SmartScreen 认证已决定暂缓，后续作为
  独立签名项目恢复；当前 unsigned Windows 已固定关闭自动 apply，并提供 `update prepare` 手动闭环；
- 补齐真实交互式 Windows 桌面的 DPI/多屏/输入法/文件选择/声音完整矩阵；
- 如发布认证要求覆盖每个 IM provider，以真实凭据补跑飞书以外渠道；当前飞书真实取消链路已通过，
  其余渠道为 deterministic mock 覆盖。

以上 gate 完成后删除本节。Windows ARM64、原生 installer、Windows Server/RDS 多会话是已确认后置
项目，不属于当前 release candidate blocker。

## 待验收：本地 Markdown Mermaid 图表的跨平台实机运行

完整实现、自动测试、本地浏览器 sandbox / 布局验证与 bundle spike 已完成，详见
`docs/plans/mermaid-rendering.md` 的实施记录。仍需在 Catalina 级 WKWebView、Windows WebView2 与
Linux WebKitGTK 分别跑一次计划 §8.3 的图型、错误、主题、Find 和回答流程矩阵；当前 macOS Tauri
Popup 会在本轮安装后先验收。实机 gate 未齐前不降低安全等级或提高系统要求。

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
