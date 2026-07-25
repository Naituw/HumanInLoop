# 需求：Dev Instance —— 多 WorkTree 并行开发隔离

> 状态：已实现；操作与 Agent 约束见 `docs/agent-worktree-setup.md`。
> 影响面：路径解析（`paths`）、CLI 入口改道、daemon 生命周期与配置/密钥加载、`install.sh`、可选 `dev` 子命令；**不改**主环境默认「每用户单 Daemon + 生产渠道」契约。

## 1. 背景

多 Agent 同时开发本项目时会撞上三层共享资源：

1. **共享二进制** `~/.local/bin/AskHuman`：`install.sh` 互相覆盖，后装者抹掉先装者的改动。
2. **共享 Daemon**（`~/.askhuman/daemon.lock` + sock）：全局单实例；二进制换新走 graceful drain，Agent 之间仍需互相等待在途提问结束。
3. **共享渠道配置 / 生产 bot**：IM 平台限制同一应用同时仅一条长连接，多 Daemon 若复用主配置会互抢。

已有 graceful drain（`docs/specs/daemon-graceful-drain.md`）解决「换新打断在途」，但**不能**解决并行开发下的串行等待与二进制互踩。perf harness 用临时 `HOME` + `ASKHUMAN_NO_KEYCHAIN` 证明了整树隔离可行，但不是产品化开发流程。

## 2. 目标

- **主环境不变**：未进入 Dev Instance 时，行为与今天完全一致（单 Daemon、生产 config/钥匙串、生产 bot）。
- **每个 Git WorkTree（含主工作树，若 enable）可有一套独立 Dev Instance**：独立二进制、独立 Daemon、独立 config/home；彼此与主环境互不 drain、互不覆盖 bin。
- **Agent 提示词零分支**：配置好某 WorkTree 后，仍只跑 `./scripts/install.sh` 与 `AskHuman …` / MCP `ask`，自动走该树的实例。
- **渠道安全默认**：Dev Instance **默认 popup-only**，绝不读主 `~/.askhuman/config.json`、绝不读主钥匙串生产密钥；需要测 IM 时在**该实例自己的** config 里配置**专用测试 bot**。

## 3. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 隔离层次 | **三层都做**：数据 home + 实例 bin + 渠道隔离 |
| D2 | 默认渠道 | **popup-only**；不读主配置/主钥匙串 |
| D3 | 测试 IM | 支持在**每个** WorkTree 实例 home 内单独配置测试 bot（多实例 = 多份 config，非共享一份「dev 渠道配置」） |
| D4 | 开启方式 | **必须显式** `dev enable`（最可预测）；不做「一进 worktree 就自动 enable」 |
| D5 | 入口改道 | 任意 `AskHuman` 从 **cwd 向上**找本树标记；命中则 **re-exec** 到该树 `.askhuman-dev/bin/AskHuman`，并带上该树 `ASKHUMAN_HOME` |
| D6 | `.askhuman-dev` 与 git | **整目录本机私有、gitignore**；不提交 bin/home/标记 |
| D7 | 多 WorkTree 模型 | **主工作树 + N 个子 WorkTree，每个可独立 enable**；每套实例数据与 bin 都落在**该工作树根**下，互不共享、互不继承 |
| D8 | Agent 流程 | enable 之后，既有「install → AskHuman 提问」提示词**不改** |
| D9 | 安装逃生口 | 已 enable 树内更新生产 bin：`./scripts/install.sh --global` |
| D10 | `dev disable` | 默认只去掉 `enabled` 并尽量 stop 本实例 daemon，**保留** `bin/`+`home/`；`--purge` 才删除整个 `.askhuman-dev` |
| D11 | 渠道预设存放 | 机器级 `~/.askhuman/dev-presets/`（主 daemon **不**加载）；每预设一文件 + `index.json` 租约 |
| D12 | 预设绑定 | `dev enable --preset <name>…`：占**独占租约**并将渠道片段**物化**进该树 `home/config.json` |
| D13 | 租约冲突 | 已被其它 worktree 占用 → enable **失败**并指明 holder；`--force` **抢租约** + stderr 强警告（一期不自动 stop 对方 daemon / 不改对方 home） |
| D14 | 僵死租约 | holder 路径不存在或已无 `enabled` 标记 → 下一次同名 preset 的 enable **自动回收**后占用 |
| D15 | 建立预设 | 主路径：`dev preset save <name> --from-instance`（从当前 dev 实例已配渠道快照）；亦支持参数/交互写入；**不提供** `--from-main` |
| D16 | 多 preset | 可重复 `--preset`；合并物化；同一 channel 出现在多个所选 preset 中 → enable 失败 |
| D17 | Agent 文档 | 新增 `docs/agent-worktree-setup.md`（准备 worktree 的步骤；用 AskHuman 问人是否挂 channel 预设）；`Agents.md` 引用：建/用 worktree 前先读该文档 |
| D18 | Dispatcher 分级 | 见计划 §2：spawn 子角色跳过改道；`dev`/help 与 `--settings` 在 bin 缺失时仍可跑（settings 写实例 home）；提问/`daemon`/`mcp` 无 bin 则失败 |

## 4. 多 WorkTree 模型（核心）

```
主环境（始终存在，与是否 enable 无关）
  bin:  ~/.local/bin/AskHuman
  data: ~/.askhuman/          ← 生产 daemon / 生产 bot

主工作树  ~/src/HumanInLoop          （可选 enable）
  .askhuman-dev/bin/AskHuman
  .askhuman-dev/home/…               ← 实例 A

子 WorkTree  ~/src/HumanInLoop-feat-x （可选 enable）
  .askhuman-dev/bin/AskHuman
  .askhuman-dev/home/…               ← 实例 B（与 A 完全独立）

子 WorkTree  ~/src/HumanInLoop-feat-y
  .askhuman-dev/…                    ← 实例 C
```

规则：

- **实例边界 = 某次 `dev enable` 时所在工作树的根目录**（见 §5 根解析），不是「整个仓库共用一个 dev」。
- 子 WorkTree 之间、子与主工作树之间：**零共享** sock/lock/config/bin/history/agents。
- 未 enable 的工作树：cwd 落在其中时仍走**主环境**（与今天相同）。
- 同一测试 bot 凭据**不应**同时写进两个已运行的实例（平台双连冲突）；产品不阻止配置，但 `doctor`/`status` 可提示「dev 实例勿共用 clientId」。默认不提供「从主配置一键导入」。

## 5. 标记与根解析

### 5.1 布局（每个已 enable 的工作树根下）

```text
<worktree-root>/
  .askhuman-dev/           # gitignore
    enabled                # 标记文件；存在即视为已 enable
    bin/
      AskHuman             # 本实例二进制（install 写入）
    home/                  # 本实例 ASKHUMAN_HOME（= config_dir 根）
      config.json          # 默认全渠道关闭
      daemon.sock
      daemon.lock
      daemon.json
      daemon.log
      history.jsonl …
      agents.json
      state/
      gui-host.sock / lock
      binhash.json
      …
```

### 5.2 工作树根

- `dev enable`：以**当前 cwd** 向上查找 Git 根（首个含 `.git` 文件或目录的祖先）作为 `<worktree-root>`；找不到 Git 根则对 cwd 报错退出（dev 仅面向本仓库工作树场景）。
- **运行时发现**（AskHuman / install.sh）：从 **进程 cwd** 向上查找名为 `.askhuman-dev/enabled` 的路径；**最近的一处命中**生效（不跨过别的工作树去「继承」父树的 enable——每个工作树根自己有自己的 `.askhuman-dev`，子 worktree 是并列目录而非嵌套在主工作树 path 下，故通常不会嵌套命中）。

### 5.3 `ASKHUMAN_HOME`

- 规范环境变量：`ASKHUMAN_HOME` 指向实例 `home/` 目录（即今日 `~/.askhuman` 的角色）。
- 所有经 `paths::config_dir()`（及等价）的落盘均相对此根。
- 未设置且未命中标记时：`config_dir()` = `~/.askhuman`（现状）。
- re-exec / install 命中标记时：强制 `ASKHUMAN_HOME=<root>/.askhuman-dev/home`。

## 6. 入口行为

### 6.1 `AskHuman`（任意已安装副本，含主 bin）

在真正执行业务前（含提问、daemon 子命令、被 MCP spawn 的路径）：

1. 若环境已显式设置 `ASKHUMAN_HOME` 且指向某 dev home，可跳过「找标记」中的 home 推导，但仍应按标记/同树 bin 做 re-exec 判定（避免 home 与 bin 不一致）。一期简化：**以 cwd 标记为准**；显式 `ASKHUMAN_HOME` 仅在无标记时覆盖 `config_dir`（高级用法，文档注明）。
2. cwd 向上找到 `.askhuman-dev/enabled`：
   - 若 `<root>/.askhuman-dev/bin/AskHuman` **不存在** → stderr 提示先在该树执行 `./scripts/install.sh`，退出码 1（禁止静默用主二进制冒充本树代码）。
   - 若存在且与 `current_exe()` 为不同路径（或内容指纹不同）→ `exec` 该 bin，argv 不变，env 设置 `ASKHUMAN_HOME=<root>/.askhuman-dev/home`，并设置 `ASKHUMAN_NO_KEYCHAIN=1`（或实例模式等价开关）。
   - 若已是该 bin → 只保证 `ASKHUMAN_HOME` 正确后继续。
3. 未找到标记 → 主环境路径，行为与今天一致。

### 6.2 `install.sh`

1. 从 cwd 向上找 `.askhuman-dev/enabled`。
2. 命中 → `INSTALL_DIR=<root>/.askhuman-dev/bin`，**不写** `~/.local/bin`；确保 `home/` 存在；若无 `config.json` 则写入默认 popup-only 配置。
3. 未命中 → 现状：安装到 `INSTALL_DIR` 默认 `~/.local/bin`。
4. 逃生口：`./scripts/install.sh --global` **强制**安装到默认全局目录，即使 cwd 在已 enable 树内（用于从 worktree 内更新生产 bin）。

### 6.3 `dev` 子命令（产品化一次配置）

| 命令 | 行为 |
|---|---|
| `AskHuman dev enable` | 在当前 Git 工作树根创建 `.askhuman-dev/{enabled,bin,home}`；seed popup-only `config.json`（若尚无）；打印「已 enable…」 |
| `AskHuman dev enable --preset <name>…` | 同上，并按 §7.2 占用预设独占租约、将渠道物化进本树 home；冲突见 D13 |
| `AskHuman dev enable … --force` | 抢占已被其它树占用的 preset 租约（警告；不自动动对方进程/home） |
| `AskHuman dev disable` | 停止本实例 daemon（若在跑）；移除 `enabled`；**释放**本树持有的 preset 租约；默认保留 `bin/`+`home/` |
| `AskHuman dev disable --purge` | 在 disable 基础上删除整个 `.askhuman-dev/` |
| `AskHuman dev status` | 是否 enable、root、home、bin、daemon、渠道摘要、本树占用的 preset 名 |
| `AskHuman dev preset save/list/show/rm/release` | 见 §7.2 |

## 7. 渠道、密钥与预设

### 7.1 每实例运行时（独立 home）

- **Seed config**：全渠道 `enabled: false`，无密钥字段。
- **加载规则**（实例模式）：
  - 只读该 home 下 `config.json`；
  - **禁止**回退读取 `~/.askhuman/config.json` / legacy；
  - **禁止**读写主钥匙串生产 secret（实例强制 no-keychain；密钥仅来自实例 config，0600）。
- **在本树改渠道**（写入当前实例 home，不影响其它树）：
  - **设置 GUI**：在本 worktree cwd 下打开 `AskHuman --settings`（经 dispatcher 进实例后，设置读写的就是本树 `home/config.json`）；
  - 或 `AskHuman channel …` / 手改 config。
- Daemon **只读本树物化后的 config**，运行时不打开 `dev-presets/`。
- **禁止触碰用户级全局登录项**（2026-07-25 补，用户实证）：`~/Library/LaunchAgents` /
  autostart 的 label 全用户唯一，实例进程写入会把**生产**的开机自启劫持到 worktree 二进制
  （launchd 以无 `ASKHUMAN_HOME` 环境重启 → 外来构建以生产 home 运行，托盘/窗口被接管），
  卸载路径则会误删生产登录项。`login_item.rs` 对全部读写入口做实例上下文守卫
  （`ASKHUMAN_HOME` 置位或 exe 含 `.askhuman-dev` 路径段 → no-op）；生产进程启动时的
  幂等同步（exe 比对）自然回写自愈。

### 7.2 机器级渠道预设（跨 WorkTree 复用模板 + 独占租约）

解决「每建一个 WorkTree 都要重新填 bot」：预设存机器级目录，enable 时点名引用；**同一预设同时只能被一棵工作树占用**（IM 长连接互斥）。

**目录**（主 daemon 不加载；权限目录 0700 / 文件 0600）：

```text
~/.askhuman/dev-presets/
  index.json           # 名 → 文件名 + lease{ worktreeRoot, claimedAt }
  <name>.json          # 渠道片段（测试 bot 字段 + 密钥明文）
```

**建立预设（少次）**

1. 推荐日常路径（`--from-instance`）：
   ```bash
   cd some-worktree
   AskHuman dev enable                 # 先空实例
   AskHuman --settings                 # 在 GUI 里配好测试飞书/钉钉/…（写入本树 home）
   # 或：AskHuman channel set …
   AskHuman dev preset save feishu-test --from-instance
   # 把「当前实例 home 里已配置/已启用的渠道片段」快照为机器级预设
   ```
2. 亦可 `dev preset save <name>` 走参数/交互录入（不经过 GUI）。
3. **不提供**从主生产 config/钥匙串一键导入（`--from-main`）。

**绑定预设（每个新 WorkTree）**

```bash
cd new-worktree
AskHuman dev enable --preset feishu-test
# 多个：--preset feishu-test --preset tg-sandbox
# 同 channel 出现在两个 preset → 失败
```

| 条件 | 结果 |
|---|---|
| 预设无 lease / lease 已是本树 | 写入 lease，物化渠道到本树 `config.json`，记录本树 `appliedPresets`（或等价元数据，供 disable 释放） |
| lease 指向其它树且对方仍 `enabled` | **失败**；stderr 给出对方 `worktreeRoot`、建议在对方 `dev disable` 或本树 `--force` |
| lease 僵死（路径不存在或对方无 `enabled`） | **自动回收**后按无 lease 处理 |
| `--force` | 抢租约到本树并物化；警告：对方 home 里可能仍留有旧密钥、对方 daemon 若仍在跑会双连，需人工处理 |

**释放**

- `dev disable`：释放本树在 `index.json` 中持有的全部 preset lease。
- `dev preset release <name>`：只清 lease，不动各树 home。
- `dev preset rm <name>`：有活跃 lease 则拒绝，除非 `--force`（先 release 再删文件）。

**与「每树独立」的关系**：预设只共享**模板 + 互斥锁**；运行配置仍是每树物化副本。两棵树要同时测真 IM，需要**两份不同预设**（两套测试 bot），不能两人挂同一 preset。

## 8. 与现有机制的关系

| 机制 | Dev Instance 下 |
|---|---|
| graceful drain | **实例内**仍生效（本树 install 换 bin → 本树 daemon drain）；**跨实例不互等** |
| 空闲退出 | 每实例独立 |
| macOS launchd | 每实例按 `ASKHUMAN_HOME` 派生独立 daemon label；与主实例、其它工作树互不 bootout；登出时随当前 GUI domain 退出 |
| GUI host | 每实例独立 sock/lock（避免与主 tray 抢）；dev 下菜单栏是否常驻可后续收紧，一期至少路径隔离 |
| Agent lifecycle hooks | 用户级全局 hook 仍可能触发；事件打到**实际 exec 到的**那个 daemon（cwd 正确则进实例）。不在一期改 hook 安装布局 |
| 自更新 | 仅主环境产品路径关心；dev bin 不走应用内 self-update 到生产目录 |

## 9. 非目标（一期）

- 不把生产 Daemon 改成多主集群。
- 不提供跨实例请求迁移。
- 不默认「附加 worktree 自动 enable」。
- 不一键复制主配置生产 bot 到实例。
- 不解决「MCP 在错误 cwd 下 spawn」的全部 harness 差异（文档说明；cwd 为 workspace root 时为一期主路径）。

## 10. 验收标准

1. 主工作树未 enable：行为与改前一致。
2. 子 WorkTree A `dev enable` 后：在 A 内 `install.sh` 只更新 A 的 bin；在 A 内 `AskHuman` 提问走 A 的 daemon；主环境在途提问不被 drain。
3. 子 WorkTree B 同时 enable + install + 提问：与 A 并行，互不等待、互不覆盖 bin。
4. A 的 config 打开测试飞书、B 保持 popup-only：互不影响；A/B 均不读取主钥匙串生产密钥。
5. 在 A 内不设任何特殊 env，仅 `AskHuman "hi"`（PATH 仍指向旧主 bin）→ 自动 re-exec 到 A 的 bin 并弹出。
6. enable 后尚未 install：`AskHuman` 明确报错，不静默走主 bin。
7. `install.sh --global` 可从 enable 树内更新 `~/.local/bin`。
8. `.askhuman-dev/` 被 gitignore，不进入版本库。
9. `dev preset save x --from-instance` 后，新树 `dev enable --preset x` 物化渠道且无需重填密钥；第二棵树再 `--preset x` 失败；`--force` 可抢；disable 释放后第二棵可占用；僵死 lease 自动回收。
10. 已 enable、**尚未** `install.sh` 时打开 `--settings`（或改实例渠道）：只读写 `<wt>/.askhuman-dev/home/config.json`；`~/.askhuman/config.json` 与主钥匙串内容不变。

## 11. 引导配置（人） vs Agent（不变）

**人 — 少次：建预设**

```bash
cd /path/to/any-worktree
AskHuman dev enable
AskHuman --settings          # GUI 配测试 bot → 写入本树 home
AskHuman dev preset save my-feishu --from-instance
```

**人 — 每个新 WorkTree**

```bash
cd /path/to/new-worktree
AskHuman dev enable --preset my-feishu   # 或不要 --preset = popup-only
```

**Agent（提示词保持）**

```bash
./scripts/install.sh
AskHuman "…"
```

---

## 反馈意见

1. **多 WorkTree**：主 + 多个子树各自独立 `.askhuman-dev`，不是全局一份 dev 配置（已写入 D7 / §4）。
2. **提示词零分支**：靠目录标记 + re-exec / install 识别，agent 仍只跑 install + AskHuman（D5/D8）。
3. **Bot 配置嫌每次手填**：要机器级预设，enable 时点名；同一预设被另一 WorkTree 占用时报错，除非 `--force`（已写入 D11–D16 / §7.2）。
4. **`--from-instance` 含义确认**：先在本 WorkTree `dev enable`，再 `AskHuman --settings`（或 channel CLI）配好测试渠道，然后 `dev preset save <name> --from-instance` 快照；不是从主生产环境抽。
5. **租约**：占用失败 / `--force` 抢租约+警告；僵死自动回收；预设目录 `~/.askhuman/dev-presets/`。
6. **Agent WorkTree 文档**：需 `docs/agent-worktree-setup.md`，并在 `Agents.md` 引用；准备新 worktree 时 agent 用 AskHuman 询问是否配置 channel 预设。
7. **Dispatcher**：A/B/C 分级 + spawn 跳过已定稿；未 install 的 settings 只写实例 home（验收 #10）。
