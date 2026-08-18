# Agent：为本项目准备 Git WorkTree（Dev Instance）

> 供 agent 在**新建或进入** git worktree 做本仓库功能开发时阅读。  
> 规格：`docs/specs/dev-instance-parallel.md`。

## 何时读本文

任务需要：

- `git worktree add …` 新建并行工作树，或  
- 在已有 worktree 里开发，且希望 **install / AskHuman 不打断主环境、不与其它 agent 互抢 daemon**  

则**先完成本文准备**，再改代码。

主环境日常使用（`~/.local/bin` + `~/.askhuman` + 生产 bot）与本文无关；未 `dev enable` 的树行为与过去一致。

## 目标体验

准备完成后，agent **提示词仍只需**：

```bash
./scripts/install.sh
AskHuman "…"   # 或 MCP ask
```

不必在提示词里分支 env / PATH；cwd 在已 enable 的树内会自动改道到该树的 bin + daemon。

Windows 对应安装命令是 `.\scripts\install-windows.cmd`；其余 `AskHuman dev …` 命令相同。

## 准备步骤

### 1. 进入 worktree 根目录

```bash
cd /path/to/your-worktree
```

### 2. 用 AskHuman 询问操作者（必须）

在 `dev enable` 前，通过 AskHuman **向人确认**：

1. 本 worktree 是否需要 **IM 测试渠道**（飞书 / 钉钉 / Telegram / Slack）？  
   - **不需要** → popup-only，`dev enable` 不带 `--preset`。  
   - **需要** → 继续下面 2a / 2b。

2a. 已有机器级预设时：

```bash
AskHuman dev preset list
```

把列表给操作者选择一个或多个名字，然后：

```bash
AskHuman dev enable --preset <name>
# 多个：--preset a --preset b
```

若报「已被其它 worktree 占用」：告知人可在对方树 `dev disable`，或确认后使用 `--force`（会抢租约；对方若仍跑着 daemon 可能双连 bot）。

2b. 还没有合适预设时：

```bash
AskHuman dev enable
AskHuman --settings          # 在 GUI 里只配【测试】bot，勿用生产凭据
# 或：AskHuman channel set …
AskHuman dev preset save <name> --from-instance
```

之后其它新树可直接 `dev enable --preset <name>`。

### 3. 安装本树二进制

```bash
./scripts/install.sh
```

在已 enable 的树内会装到 `.askhuman-dev/bin/`，**不会**覆盖 `~/.local/bin`（除非 `./scripts/install.sh --global`）。

Windows PowerShell / cmd：

```powershell
.\scripts\install-windows.cmd
# 显式更新生产安装（逃生口）：.\scripts\install-windows.cmd -Global
```

Windows 默认安装同样只写 `.askhuman-dev\bin\AskHuman.exe`，不会修改生产 EXE、用户 PATH 或受管
`WindowsApps\AskHuman.cmd` launcher。

### 4. 自检（可选）

```bash
AskHuman dev status
AskHuman daemon status    # endpoint 应按本树 .askhuman-dev/home/ 独立派生
```

## 禁止事项

- **不要**把生产 bot 凭据写进 dev 实例，也不要指望从主配置一键导入（产品不提供）。  
- **不要**在两棵树上同时 `enable --preset` 同一预设（平台长连接互斥）；需要并行真 IM 时准备**两套**测试 bot / 两个预设。  
- **不要**为了「走 dev」去改 agent 全局提示词分支；靠目录标记自动改道。

## 结束 worktree 时（可选）

```bash
AskHuman dev disable           # 停实例 daemon、释 preset 租约、去 enabled；保留 bin/home
AskHuman dev disable --purge   # 连 .askhuman-dev 目录一起删
```

## 故障简表

| 现象 | 处理 |
|---|---|
| `binary is missing` | 在本树跑 `./scripts/install.sh`；Windows 跑 `.\scripts\install-windows.cmd` |
| 提问仍进主 daemon | 确认 cwd 在 enable 的树内；PATH 上的 `AskHuman` 是否已含 dispatcher（至少全局装过一版本功能） |
| preset 占用冲突 | 对方 `dev disable` 或本树 `--force` |
| 设置改到了生产 config | 未 enable；先 `dev enable` 再 `--settings` |
