# 需求：版本自更新机制（self-update）

> 状态：已实现；macOS/Linux 支持 Direct/npm 自动 apply，Windows 支持检查、日志与安全的手动更新准备。
> 关联计划：`docs/plans/self-update.md`
> 影响面：新增 `update/` 模块；daemon 生命周期与 IPC 协议（增量，PROTOCOL_VERSION 不变）；前端弹窗 `PopupView` 与设置 `SettingsView`；`commands.rs`；`release.yml` + 新增 `cliff.toml`；发布流程（更新日志生成）。**不改**正常提问流程的 stdout 契约、退出码语义。

> **实现期补充**：GitHub API 客户端支持 `ASKHUMAN_GITHUB_TOKEN` / `GITHUB_TOKEN` 鉴权，403/429
> 统一标记为 rate-limited 并向前端给出手动下载/配置 token 的友好提示；普通 npm registry 与资产下载
> 客户端不携带该鉴权头。
>
> **主动更新换新时效补充（2026-07）**：应用内更新成功落盘后，除保留 daemon 的 15 秒指纹轮询
> （覆盖外部 npm/install 更新）外，还会立即向**已运行** daemon 发送更新快照刷新 + 常规 Hello
> 指纹握手；空闲 daemon 立即退出换新，有在途请求则立即进入 graceful drain，未运行 daemon 不会仅
> 因同步更新而被拉起。菜单栏继续显示真实的运行中 daemon 版本，并在其下按阶段解释目标版本与等待
> 原因（含在途数量及“不打断作答”）。
>
> **Windows 未签名发行补充（2026-08）**：Direct/npm 的事务 worker、回滚、WinVerifyTrust 与版本校验
> 代码均已实现并保留，但当前 Windows release 未配置生产 Authenticode。源码 capability 固定关闭所有
> 联网自动 apply 入口，界面只提供手动路径。`AskHuman update prepare` 会 graceful drain daemon、关闭
> GUI Host 并释放 `.exe` 锁；Direct installer 仍可复用本地事务 worker 安装显式选定的 build output。

## 1. 背景与动机

设计本功能时，升级只能靠手动重装（`npm i -g` 或 `install.sh`）。本功能为应用增加检查、日志与更新提示；
macOS/Linux 提供一键更新，Windows 未签名阶段提供可执行的手动更新路径。所有平台都**不能打断**用户
正在进行的作答。

daemon 已具备**优雅排空换新（graceful drain）**：盘上二进制内容指纹一变，daemon 会在「在途请求全部完结后」自动退出，由下一个请求拉起新二进制（见 `docs/specs/daemon-graceful-drain.md`）。Unix 自动 apply 可原子替换盘上文件并交给 drain 生效；Windows 手动安装前由 `update prepare` 先完成同样的排空与后台退出。保留的目录外事务 worker 供 installer 与未来签名自动更新复用。

参考实现：`/Users/wutian/Developer/humanInLoop-rust`（`src/rust/ui/updater.rs`、`cliff.toml`、`release.yml`、`useVersionCheck.ts`）。本方案借鉴其 GitHub-Release 下载替换与 git-cliff 更新日志，但**用 daemon drain 取代其 `app.restart()`**。

## 2. 目标（一句话）

应用内可检查和查看更新；macOS/Linux 可自动应用，Windows 未签名阶段在排空后台后由用户手动安装。
全程不打断作答，任何不满足策略的自动 apply 均在网络与文件操作前 fail closed。

## 3. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 平台范围 | **macOS + Linux + Windows x64**。Unix 由 drain 协调自动原子替换；Windows 未签名阶段仅检查并手动安装，`update prepare` 协调运行中 exe、daemon 与 GUI Host。 |
| D2 | 新二进制来源 / 安装方式分流 | **检测安装方式、分别更新**：按 daemon `current_exe()` 路径判定——含 `/node_modules/@humaninloop/` 或 `/node_modules/askhuman/` → **npm 安装**；否则 → **直装二进制**（`install.sh` / 手动下载，如 `~/.local/bin`）。 |
| D3 | 两套更新实现 | 统一抽象 `Updater`（`check_latest()` + `apply()`），运行时按 D2 选择：① **DirectUpdater**：GitHub Releases 查版本 + 下载平台资产替换；② **NpmUpdater**：npm registry 查版本 + 跑 `npm i -g askhuman@latest`。 |
| D4 | npm 更新点击行为 | macOS/Linux 点「更新」自动执行 `npm i -g askhuman@latest`。Windows 显示并可复制该固定命令，二次确认后只运行 `update prepare`，不会自动运行 npm。保留的 Windows npm worker 只有将来完成正式签名项目后才可重新启用。 |
| D5 | 生效时机（关键，无用户开关） | Unix 更新由用户主动触发，落盘后用 Hello 指纹握手触发 daemon drain。Windows 使用源码固定 capability，不能由配置/环境变量打开自动 apply；`update prepare` 等在途清零后关闭 daemon/GUI Host，用户随后安装。两者都**绝不打断当前作答**。 |
| D6 | 外部更新也提示 | daemon 已监听二进制指纹变化；**任何来源**导致盘上二进制变化（如用户在终端跑了 `npm i -g`、另一处装了新版）都触发「待生效」提示——不限于应用内点更新。 |
| D7 | 版本门槛 | 仅当「远端正式版 > 本地 `CARGO_PKG_VERSION`」才提示更新（数字段逐段比较）。故本地同版本开发构建不会被误判为有更新。 |
| D8 | 检查时机 / 频率 | daemon **启动时 + 周期性（约每 24h）** 后台检查；设置与菜单栏内可**手动「检查更新」**。手动检查要求上游缓存重新验证，避免刚发布的版本仍命中旧 `/latest` 响应。检查结果（最新版本、检查时间）落 `~/.askhuman/update.json`；多进程读改写由 `update.lock` 串行化，已观察到的最高版本不会被短暂旧响应降级。 |
| D9 | 「忽略此版本」 | 某版本被用户忽略后**不再主动弹该版本**（设置里仍可见、可更新）；**手动检查重置**忽略记录。忽略集合持久化（随 `update.json` 或前端 localStorage，见计划）。 |
| D10 | 检查 / 广播归属 | **daemon** 负责后台周期检查、存状态、并向**所有打开的弹窗** GUI Helper 广播「有更新 / 待生效」；弹窗经 IPC 读取/接收。**设置窗 / GUI Host 自行**按需检查与触发更新；成功后立即刷新 Host 内存，并以 best-effort IPC 让已运行 daemon 重载同一持久化快照、转播给弹窗。daemon 停止时不为同步结果而额外启动。 |
| D11 | 弹窗 UI | 有更新时显示版本与日志。Unix 显示自动更新；Windows 显示“查看手动更新步骤”，只打开设置定位，不从在途 Popup 排空或关窗。 |
| D12 | 设置 UI（关于区） | 设置「通用」Tab 显示当前/最新版本与日志。Windows 显示中性手动说明、Direct release 链接或 npm 命令，以及二次确认的“准备手动更新”按钮。 |
| D13 | 设置内更新后看新版 | 更新完成后设置窗显示「**重启设置页面**」按钮（文案不带括注）：用新二进制重启设置进程（spawn 新 `--settings` 后退出当前窗），立即看到新设置项。设置是独立进程，重启它**不影响**任何在途弹窗 / 作答。 |
| D14 | 无弹窗时在设置点更新 | 落盘替换后：若 daemon 空闲（无在途）→ 即时空闲换新（下次提问即新版）；设置窗提示「已更新，下次提问生效」。若此刻别处有在途弹窗 → 照常等其答完再换（drain）。 |
| D15 | 更新日志生成 | 采用 **git-cliff**（`cliff.toml`）从 conventional commits 自动分组生成，发布时零整理。**只纳入** `feat`/`fix`/`perf`（+`security`/`revert`）；**跳过** `chore`/`docs`/`style`/`refactor`/`test`/`ci`/`build`。**单条覆盖**（commit footer trailer）：`Release-Note: <文案>` 用该文案替代标题、`Release-Note: skip` 强制排除该条。 |
| D16 | 更新日志格式 | emoji 分组标题（⚠ Breaking Changes / ✨ Features / 🐞 Fixes / 💎 Performance / 🔒 Security / ⏪ Revert）；**breaking（`type!:` 或 `BREAKING CHANGE:` footer）单列『⚠ Breaking Changes』并置顶**；去掉 `type(scope):` 前缀、**scope 转粗体前缀**保留；句首大写；附 `Full Changelog` 对比链接。**英文为主**（沿用英文 commit）；中英双语（AI 生成）作为后续增强。 |
| D17 | 更新日志覆盖机制 | 发布时若存在 `docs/release-notes/v<版本>.md` → **用它作 Release body**；否则自动用 git-cliff 生成。（将来 AI 预生成的日志丢入该文件即可被采用。） |
| D18 | 更新日志展示来源 + 聚合 | 展示统一从 **GitHub Releases 按 tag** 取 body；**聚合懒加载**：后台「检查」只取 `releases/latest`（或 npm registry，1 请求，快）；只有用户**展开查看日志**时才拉 `releases` 列表、聚合「当前版本→最新版本」之间所有版本并缓存——不拖慢检查。npm 安装时版本走 npm registry，日志仍按 tag 从 GitHub 取（取不到则「暂无更新日志」占位）。 |
| D19 | 下载安全 | 保留上一版 `.bak` 便于回滚；Unix 用同目录临时文件 + 原子替换，macOS 校验 Developer ID 与 TeamID。Windows 当前 release 未签名，故运行时不下载、不执行、不替换 release EXE；`SHA256SUMS` 仅证明完整性，不能代替 publisher authenticity。WinVerifyTrust、事务与 verifier 代码保留给 installer/未来签名项目。 |
| D20 | 提交信息规范 | 扩写 `AGENTS.md` 的「Commit messages」为完整 **Conventional Commits** 规范：`<type>(<scope>): <subject>`；type 与归类同 D15；scope 可选但鼓励（小写区域名、逗号分隔，日志中作粗体前缀）；subject 英文/祈使句/小写起/无句号/≤72；breaking 用 `type!:` 或 `BREAKING CHANGE:` footer；支持 `Release-Note:` / `Release-Note: skip` 单条覆盖。**并明确说明：这些信息会进入用户可见的 release notes，必须认真撰写。** |

## 4. 约束与既有规则（不可破坏）

- **不打断在途**：更新只落盘，换新交给既有 drain；任何在途弹窗 / IM 卡片 / 抢答语义零变化。
- **不 restart 提问进程**：与参考实现的 `app.restart()` 不同。Unix 设置进程的「重启设置页面」是独立进程、与 drain 无关；Windows worker 只在排空后协调退出/恢复后台角色和调用它的设置/宿主进程，不会重启在途提问进程。
- **stdout 洁净 / 退出码契约**不变；更新相关日志走 stderr / daemon.log。
- **后台网络失败静默**：daemon 周期检查失败不打扰用户；设置或菜单栏内的手动检查可在当前界面显式报错。
- IPC 为**增量演进**：PROTOCOL_VERSION 保持 1，新枚举值 / 字段对旧端可降级（参照 graceful-drain 的兼容做法）。
- Windows 自动 apply 固定关闭；任何旧 UI/IPC/trait 调用都必须在网络、临时目录、pending 状态和 drain
  之前拒绝。Direct `__update-worker` 仍供显式本地 installer 使用，不得误删或绕过其 hash/version 事务。

## 5. 验收标准

1. **直装二进制**：macOS/Linux 保持自动下载、校验和 drain；Windows Popup 显示日志并跳到手动步骤，设置可打开 Release 且启动 prepare，不下载或执行新 EXE。
2. **多弹窗待生效**：A、B 两个在途弹窗，其一触发更新 → 两个弹窗都出现「新版本将在所有弹窗回复完成后生效，请尽快回复」提示条；全部答完后 drain 换新。
3. **外部更新提示**：在终端执行 `npm i -g askhuman@latest`（或另处装新版）→ 已打开的弹窗同样显示「待生效」提示（D6）。
4. **npm 安装方式**：macOS/Linux 自动运行 npm；Windows 显示/复制 `npm i -g askhuman@latest`，prepare 返回 0 后由用户在终端执行。
5. **设置关于区**：显示当前 / 最新版本、可手动「检查更新」、渲染更新日志（多版本跳跃时聚合显示，附「查看全部发布」）；忽略某版本后不再主动弹、手动检查可重置（D9）。
6. **设置内更新**：Unix 自动更新保持原行为；Windows 二次确认后等待在途请求、关闭后台并释放文件锁，失败返回非 0 且不误报已更新。
7. **安全**：macOS 校验签名 / TeamID；Windows 所有自动 apply 入口 fail closed，未签名 release 不被运行时执行。
8. **发布流程**：打 tag 发布时，Release body 由 git-cliff 生成（仅 feat/fix/perf 等、emoji 分组、scope 粗体、Full Changelog 链接）；若存在 `docs/release-notes/v<版本>.md` 则改用其内容（D15/D16/D17）。
9. **门槛 / 回归**：本地等于或高于最新版时不提示更新（D7）；正常提问 / drain / 设置 / 历史等既有功能不受影响。
10. **Windows 手动闭环**：`AskHuman update prepare` 在 daemon 未运行/运行/有在途与 GUI Host 打开时均可靠处理；返回 0 后 Direct/npm 可手动替换，下次调用按既有 lazy-start 拉起新 daemon。保留的事务 worker 继续通过 installer/负向测试。
