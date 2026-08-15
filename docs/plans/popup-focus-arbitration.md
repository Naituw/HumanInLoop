# 实现计划：并发 Popup 焦点仲裁与后方级联

> Windows 注（2026-08）：本文的 Windows 单进程范围声明是首期历史记录；Windows 已接入 shared
> daemon/GUI Host，焦点仲裁复用同一业务状态机，桌面实机矩阵仍是发布验收 gate。

> 状态：已实现；macOS 真机验收与自动化验证通过。Linux 保留同构仲裁与 best-effort 展示路径，按确认不另记真机验收待办。
> 范围：macOS 完整实现；Linux 复用 daemon 仲裁与级联位置策略，按窗口管理器能力兼容。
> Windows 当前走非 Unix 单进程回退路径，不在本轮范围内。

## 1. 目标与已定案交互

多个 Agent 并发调用 AskHuman 时，Popup 仍各自使用独立 GUI Helper 进程，但只能有一个 Popup 拥有
“自动前置并取得键盘焦点”的权利：

1. 第一个实际派发成功的 Popup 成为主答窗口，按现状居中、前置并聚焦。
2. 后续 Popup 立即显示，但不得激活应用或取得键盘焦点；它们显示在主答窗口后方，并按 macOS 常见的
   级联方式向右下错开，使边缘可见、正文不覆盖当前编辑窗口。
3. 主答窗口作答、取消、被 IM 抢答、调用方断开或窗口异常退出后，按请求创建顺序把焦点交给最早的
   存活等待窗口。
4. 用户主动点击等待窗口，或从托盘“待答”菜单选择某请求时，视为明确切换：该窗口立即成为新的
   主答窗口；原主答窗口回到等待队首。
5. 保护周期是“主答窗口取得所有权后直到完成或用户明确切换”，不以 textarea 的 DOM focus 为判据。
   用户复制问题、选择附件或短暂让输入框 blur 时，新请求仍不得打断当前操作。
6. 提示音、Dock 跳动/角标、出现动画、置顶配置、预热与冷启动行为保持；本功能只改变并发窗口的
   首次聚焦、层级、位置和接力。

### 1.1 非目标

- 不把多个请求合并到同一个窗口、Tab 或列表中。
- 不改变普通 Ask 与结构化 Permission Confirm 的回答、取消或抢答语义。
- 不修改 stdout、退出码、历史记录和 IM 卡片协议。
- 不根据用户是否正在输入字符动态抢占；焦点所有权只因请求终态或明确用户切换而变化。
- 本轮不为 Windows 新建跨进程 daemon/宿主仲裁机制。

## 2. 现状约束与根因

- 每个请求对应一个短命 GUI Helper；并发 Popup 不在同一 Tauri `AppHandle` 中，不能靠进程内全局变量
  判断其它窗口。
- 冷路径在 `app/mod.rs::launch` 中 `show()` 后对 helper 调 `set_focus()`；热路径在
  `finalize_popup_show()` 中同样无条件 `show()` + `set_focus()`。
- 当前 Tauri 2.11.5 / TAO 0.35.3 的 macOS `show()` 底层调用 `makeKeyAndOrderFront`，所以仅删除显式
  `set_focus()` 仍会抢焦点。等待窗口必须走 AppKit 的非 key 排序路径，不能调用普通 Tauri `show()`。
- daemon 的 `RequestRegistry` 已有单调递增 `seq`，并已有按 request_id 下发 `FocusPopup` 的能力；这两项
  分别可复用为 FIFO 顺序和接力前置通路。
- 冷 helper 在建窗前已收到 `Show`；热 helper 在隐藏窗口挂载后才收到 `Show`。要可靠取得原生窗口编号、
  跨进程排序并覆盖两条路径，必须把“内容下发”和“允许上屏”拆成两个阶段。

## 3. 总体架构

daemon 新增一个纯状态机 `PopupFocusArbiter`，作为所有 Popup 自动聚焦与级联顺序的唯一权威。GUI Helper
只执行 daemon 下发的展示动作，不自行查询“当前是否已有其它 AskHuman 窗口”。

```text
request accepted
  -> reserve(request_id, seq)              # 按创建顺序占位
  -> dispatch cold/warm helper
  -> ServerMsg::Show                       # 只下发内容，窗口仍隐藏
  -> ClientMsg::PopupReady(window metadata)# 内容和原生窗口均就绪
  -> arbiter decides presentation
       owner  -> PresentPopup::Foreground
       waiter -> PresentPopup::BackgroundCascade
  -> GUI performs native presentation

owner terminal
  -> request marked terminal
  -> GUI window dismissed / helper disconnected
  -> arbiter promotes oldest live waiter
  -> existing ServerMsg::FocusPopup
```

仲裁器只输出动作，不直接调用 Tauri、AppKit 或写 socket。daemon 外层按动作找到对应 GUI sender 并执行，
使状态机可以做完整的无 GUI 单元测试。

## 4. daemon 焦点状态机

### 4.1 数据模型

建议新增 `src-tauri/src/daemon/popup_focus.rs`：

- `owner: Option<RequestId>`：当前自动聚焦所有者。
- `entries: HashMap<RequestId, PopupSurface>`：只登记实际准备派发 Popup 的 Ask/Confirm。
- `order: VecDeque<RequestId>`：除 owner 外的等待顺序；初始按 `RequestEntry/ConfirmEntry.seq`。
- `PopupSurface` 至少记录：
  - `seq`；
  - `phase: Reserved | Ready | Presented | Terminal`；
  - GUI 是否仍存活；
  - macOS 原生 `window_number`（其它平台为 `None`）；
  - 当前级联槽位 / 前驱窗口编号；
  - 请求是否终态、窗口是否已销毁。

状态机对重复 ready、重复 focused、重复 terminal、迟到 dismissed 必须幂等；不得因 socket 重复帧产生双重
晋升或队列重复项。

### 4.2 核心操作

1. `reserve(request_id, seq)`
   - 在真正 dispatch 前调用，避免两个 helper 并发连回时按“就绪速度”反转先后顺序。
   - 无 owner 时设为 owner；否则按 `seq` 入等待队列。

2. `dispatch_failed(request_id)`
   - spawn/热池 assign 失败时立即撤销占位。
   - 若失败项是 owner，晋升最早的等待项；不能留下不可见的幽灵 owner。

3. `ready(request_id, metadata)`
   - owner 就绪：输出 `PresentForeground`，再依次放行已就绪的等待窗口。
   - waiter 就绪且前驱窗口已就绪：输出 `PresentBackgroundCascade`。
   - waiter 比 owner/前驱更早 ready：保持隐藏；前驱 ready 后再统一产生展示动作，确保不会先闪到前台。

4. `claim(request_id)`
   - 用于原生 `Focused(true)` 和托盘 `FocusRequest`。
   - 目标已是 owner：只返回普通 focus/flash 动作。
   - 目标是 waiter：从队列移除目标，把旧 owner 放到等待队首，再把目标设为 owner。
   - 用户主动选择优先于 FIFO，但其余等待项相对顺序不变。

5. `terminal(request_id)` + `dismissed(request_id)`
   - 请求终态先标记，不立即让下一个窗口和尚未消失的当前窗口争抢前台。
   - 窗口确认销毁后才移除 owner并晋升最早存活 waiter。
   - helper socket EOF/崩溃视为窗口已不可继续使用，可立即释放。
   - 为防平台漏发 Destroyed，终态后设置 750ms 超时兜底；正常路径仍由 dismissed 即时接力，超时只释放
     焦点租约，不改变请求结果。

6. `surface_disconnected(request_id)`
   - 无论请求是否仍可由 IM 回答，都从 Popup 仲裁中移除该 surface。
   - owner surface 断开时晋升下一 Popup；不能让一个仅剩 IM 的请求阻塞所有本地窗口。

### 4.3 锁与消息顺序

- `PopupFocusArbiter` 使用 daemon 内单一 `Mutex`；在锁内只更新状态并收集 `Vec<PopupEffect>`，释放锁后
  才向 GUI channel 发送，避免锁内 I/O 和反向调用。
- `Show`、GUI sender 注册和 ready/presentation 的次序必须有一个统一入口，不能出现 `FocusPopup` 先于
  `Show` 被冷 helper 丢弃的竞态。
- 请求注册表移除路径统一调用一个收尾函数；普通 Ask、Confirm、CLI EOF、无渠道、watchdog、daemon
  cancel-all 都不得各自遗漏焦点释放。

## 5. IPC 增量与两阶段展示

### 5.1 GUI -> daemon

在 `ClientMsg` 增加：

- `PopupReady { request_id, window_number, ... }`
  - 前端内容已注入、原生窗口已构建但仍隐藏时发送；每个 helper 只发一次。
  - macOS 携带全局 `NSWindow.windowNumber`；Linux/其它平台字段为 `None`。
- `PopupFocused { request_id }`
  - `WindowEvent::Focused(true)` 时发送，供用户直接点击等待窗口后转移 owner。
- `PopupDismissed { request_id }`
  - `WindowEvent::Destroyed` 时发送，供终态后的无闪烁接力。

所有消息校验 `request_id` 必须与连接绑定 entry 一致；daemon 不信任 GUI 自报的其它请求 ID。

### 5.2 daemon -> GUI

新增：

- `PresentPopup { request_id, presentation }`
- `presentation = Foreground | BackgroundCascade { cascade_index, behind_window_number }`

`Foreground` 允许正常激活；`BackgroundCascade` 必须非激活显示并排在前驱窗口后方。继续复用既有
`FocusPopup` 处理已经显示的等待窗口接力和托盘显式聚焦，不另造第二套 focus 消息。

### 5.3 兼容边界

- 新 daemon 只会拉起同一安装版本的 helper，预热 helper 在 drain/二进制换新时会被回收，因此本轮作为
  同版本内部协议增量，`PROTOCOL_VERSION` 不需要仅为这些枚举变体升级。
- 新字段使用 serde default/optional，避免诊断工具或测试 fixture 因缺字段失败。
- `ShowPayload` 继续只承载请求内容；展示权留在 `PresentPopup`，防止 owner 在 helper 建窗前变化后携带
  过期决策。

## 6. GUI Helper 改造

### 6.1 冷/热路径统一 ready

- 冷路径不再在 `setup` 中直接 `show()`；窗口仍按现状隐藏构建、设置材质/动画、加载请求。
- 热路径保留“领用后 nextTick，确保本次内容已进入 DOM”的规则。
- 两条路径最终都只调用一次 `popup_ready_to_present`：
  1. 收集原生窗口 metadata；
  2. 经 `GuiBridge` 发送 `PopupReady`；
  3. 等 daemon 回 `PresentPopup` 后才上屏。
- `PopupReady` 到 `PresentPopup` 的等待不依赖 rAF；隐藏预热窗口没有 display link，不能重引入
  “先等双 rAF 再 show”导致永不上屏的问题。
- dispatch 后 10s 仍未收到 `PopupReady` 时，沿用 GUI 启动失败的处理原则：从 Popup 仲裁移除该 surface；
  无 IM 可用时让请求按现有 popup failure 结果收尾，有 IM 时保留请求并继续等待远端回答。

### 6.2 统一展示函数

把当前冷路径和 `finalize_popup_show()` 的重复逻辑收敛为一个接受 `PopupPresentation` 的主线程函数：

- 共用部分：读取最新 config、重设 size/always-on-top/theme/effect、出现动画、Dock 图标与角标、提示音、
  `gui.win_show` 性能打点。
- `Foreground`：沿用正常 Tauri `show()` + `set_focus()`。
- `BackgroundCascade`：先计算并设置级联位置，再走平台非激活显示；严禁随后无条件 `set_focus()`。
- 展示后窗口仍是普通可聚焦窗口；不能永久 `set_focusable(false)`，否则用户无法点击切换。

## 7. macOS 后方级联实现

### 7.1 原生辅助模块

建议新增 `src-tauri/src/macos_window_order.rs`，沿用现有 `macos_window_anim.rs` 的 objc2 风格，代码注释使用
英文。核心能力：

- 读取 `NSWindow.windowNumber`；
- 用 `cascadeTopLeftFromPoint:` 应用 AppKit 原生级联位置；
- 用 `orderWindow:NSWindowBelow relativeTo:<前驱 window number>` 非激活地显示窗口；
- foreground 接力继续由 Tauri `set_focus()` 完成，其底层会 `activateIgnoringOtherApps`。

`BackgroundCascade` 不调用 Tauri `show()`，因为当前 TAO 的 macOS `show()` 会
`makeKeyAndOrderFront`。切换预热 helper 的 activation policy、重设 Dock icon 本身也要真机确认不会激活应用。

### 7.2 级联规则

- owner 保持首次居中位置。
- 每个后继使用 AppKit 系统原生级联位移；`cascadeTopLeftFromPoint:` 的第一次调用只锚定起点，因此
  `cascade_index=1` 需要执行到第二次调用才产生第一个可见错位。
- 屏幕边缘与可见区域约束交给 AppKit 的系统级联行为处理，不另设固定 24pt/40pt 步长。
- 已显示窗口不因队列前方窗口完成而整体跳动；接力只改变层级和焦点，避免用户看到窗口重排。
- 多显示器下以 helper 构建时所在屏幕的 visible frame 约束；本轮不跨屏搬移等待窗口。

### 7.3 原生技术探针（实现第一步）

在铺开 IPC 前先用两个真实 helper 验证：

1. 跨进程 `NSWindow.windowNumber` 能否作为 `orderWindow:relativeTo:` 的参照；
2. `NSWindowBelow` 能否在双方均 always-on-top 时保持 owner 在前；
3. background show 不改变 key window、active application、文本输入落点；
4. warm helper 从 Accessory 切回 Regular 时不因 policy 切换单独抢焦点；
5. 出现动画、Liquid Glass/材质、Dock 角标仍正常。

若跨进程 relative ordering 在目标 macOS 版本不可用，保持产品语义不变，原生实现回退为同窗口级别的
`orderBack` + 级联错位；不得回退为 Tauri `show()` 后再抢回旧窗口，因为那会产生可感知焦点闪烁。

真机探针结果：跨 helper 的全局 window number 可用于 `NSWindowBelow` 相对排序；后方窗口不会激活应用
或夺取当前文本输入焦点。系统级联位移经三窗口验证可见，最终选择保留 AppKit 原生步长。

## 8. Linux 兼容路径

- daemon 使用完全相同的 owner/FIFO/manual-claim 状态机和 `PresentPopup` 协议。
- waiting helper 设置级联位置，普通显示时不调用 `set_focus()`；builder 使用非初始聚焦属性。
- X11/Wayland 对跨进程 z-order 和 focus-stealing prevention 的支持取决于窗口管理器。本轮要求代码不主动
  请求焦点并保持可点击，精确“位于 owner 后方”作为 best effort；macOS 是完整行为基线。
- headless/no DISPLAY 继续沿用现有不派发 Popup 的逻辑，不进入仲裁队列。

## 9. 生命周期与异常收尾

### 9.1 正常作答/取消

- helper 先送 Answer/ConfirmAnswer 并关闭窗口；daemon 将请求标为 terminal。
- `PopupDismissed` 或连接 EOF 到达后释放 owner并前置下一窗口。
- CLI 的 final 输出不必等待视觉接力完成；焦点收尾作为 daemon 内独立、短时有界的动作。

### 9.2 IM 抢答与调用方断开

- daemon 先通知当前 helper 关闭，再把仲裁项标记 terminal。
- 收到 dismissed/EOF 后接力；超时兜底保证 helper 异常时队列不会卡死。

### 9.3 helper 启动失败/崩溃

- dispatch 失败立即 rollback reservation。
- 已展示 helper 断连立即移除 surface；请求若仍有 IM 渠道可继续等待，但不再占 Popup owner。
- warm helper 领用/补热逻辑不变；焦点占位属于请求 surface，不属于热池进程本身。

### 9.4 daemon drain/stop

- cancel-all 对全部 surface 标 terminal，并停止产生新的自动 focus 动作。
- drain 等在途结束期间仍允许现有 owner/等待队列正常接力；真正 shutdown 后 helper 断连自行退出。
- 新二进制启动时状态从空开始，不跨 daemon 重启持久化焦点队列。

## 10. 实施阶段

### Phase 0：macOS 原生探针（完成）

1. 写最小原生窗口编号、非激活排序和级联位置辅助代码。
2. 用两个独立 Popup helper 验证 §7.3；记录所选 AppKit 调用与 macOS 版本结果。
3. 探针通过后再固化 IPC；若需使用 `orderBack` 回退，只替换原生实现，不改变后续状态机设计。

### Phase 1：纯仲裁器与测试（完成）

1. 新建 `popup_focus.rs`，实现 reserve/ready/claim/terminal/dismissed/disconnect 状态机。
2. 让状态机输出纯 `PopupEffect`，补齐并发、迟到、重复事件单测。
3. 把 Ask/Confirm 的 popup dispatch 成功/失败统一接入 reservation。

### Phase 2：IPC 与冷/热两阶段展示（完成）

1. 增加 `PopupReady`/`PopupFocused`/`PopupDismissed`/`PresentPopup`。
2. 冷路径取消 setup 内直接 show；冷/热统一在内容 ready 后报到。
3. daemon 原子登记 GUI sender、处理 ready 并下发展示动作，封住 Show/Focus 乱序窗口。
4. 保留 warm 的 nextTick 上屏前内容保证与性能打点语义。

### Phase 3：macOS 完整展示与手动切换（完成）

1. 接入 foreground/background 两种原生展示。
2. 实现后方排序与 AppKit 系统级联错位。
3. 接入 `WindowEvent::Focused(true)` 与托盘 `FocusRequest` 的 owner 转交。
4. 验证 always-on-top 开/关、预热开/关、动画/材质/声音/Dock 均不回归。

### Phase 4：终态确认与 Linux 兼容（完成；Linux 展示为 best effort）

1. 接入 dismissed/EOF/超时三层释放，收敛所有 request removal 分支。
2. 覆盖 IM 抢答、CLI EOF、watchdog、Confirm fallback、daemon cancel-all。
3. 实现 Linux 非主动聚焦 + 级联位置 best-effort 路径。

### Phase 5：验证、安装与文档收尾（完成）

1. 跑完整自动化矩阵。
2. `./scripts/install.sh` 编译并安装新二进制。
3. 使用新安装的 AskHuman 做双/三 Popup 真机验收。
4. 实现完成后更新 `docs/overview-popup-ui.md` 的多窗口行为；主 `docs/overview.md` 的仓库级进程模型未变，
   无需修改。
5. 若仍有明确未验收平台/场景，写入 `docs/PROGRESS.md`；全部完成则不新增进度项。

## 11. 自动化测试矩阵

### 11.1 `PopupFocusArbiter` 单元测试

| 场景 | 预期 |
|---|---|
| 单请求 reserve -> ready | foreground，行为与现状一致 |
| 两请求按 seq reserve、第二个先 ready | 第二个保持隐藏，第一 ready 后按顺序展示 |
| owner ready，第二/第三 ready | owner foreground；二、三按链后方级联 |
| owner terminal 但未 dismissed | 暂不 focus 下一窗口 |
| owner dismissed/EOF | FIFO 晋升并只发一次 FocusPopup |
| owner dispatch 失败 | 最早 waiter 晋升，不留幽灵 owner |
| waiter dispatch 失败/断开 | 从队列移除，其余顺序不变 |
| 用户点击 waiter | waiter 成 owner，旧 owner 回等待队首 |
| 托盘选择尚未 ready 的 waiter | 先转交 owner，ready 后 foreground |
| IM 抢答 owner | 关闭确认后接力；请求结果不受焦点状态影响 |
| 重复 ready/focused/terminal/dismissed | 幂等，无重复队列和重复 focus |
| Ask 与 Confirm 混排 | 统一按 seq 仲裁，不按交互类型分队列 |

### 11.2 IPC/后端测试

- 新消息 serde 往返、optional metadata、非法 request_id 被忽略。
- cold/warm 都只报一次 ready；background presentation 不走 `set_focus()`。
- Show 永远先于同连接的 Present/Focus；迟到的 Focus 不会在 cold 握手阶段丢失。
- request 所有终态路径最终都从 arbiter 移除。
- Linux 条件编译与 macOS 原生模块条件编译均通过。

### 11.3 前端回归

- warm 领用仍在内容 nextTick 后报 ready，隐藏状态不等待 rAF。
- cold init、附件加载、Permission diff 异步增强不因延后上屏而阻塞回答按钮。
- 现有 Popup Vitest、`pnpm build`/vue-tsc 全部通过。

## 12. 真机验收

macOS 至少覆盖：

1. 在第一个 Popup textarea 持续输入，同时触发第二个请求：键盘焦点和输入字符始终留在第一个窗口。
2. 第二、第三个 Popup 在后方右下级联，仅露出边缘，不覆盖主答正文；声音和 Dock 提示仍出现。
3. 完成第一个后第二个只前置一次并取得焦点；再完成后第三个接力。
4. 主动点击第三个或从托盘选择第三个：第三个立即成为 owner；完成后回到先前主答窗口。
5. owner 被取消、调用方终止、IM 抢答、helper 强退时，下一窗口都能接力且无队列卡死。
6. 冷 helper、预热 helper、并发导致的冷回退三条路径行为一致。
7. `alwaysOnTop=true/false`、不同窗口尺寸、屏幕边缘、多显示器、切换 Space 后无焦点闪烁或窗口越界。
8. 普通 Ask 与 Permission Confirm 并发时同样遵守一条全局 FIFO。

Linux 至少做一次双 Popup 验收：新窗口不由代码显式请求 focus、可手动点击切换、owner 完成后 FIFO 接力；
记录桌面环境/显示协议和窗口管理器对后方排序的实际支持。

本轮验收记录：macOS 三窗口验证确认后续窗口在 owner 后方显示且不抢焦点，系统原生级联错位正常，
关闭当前 owner 后连接均能正常收尾。Linux 路径通过条件编译和共享状态机测试；按最终确认以 best-effort
兼容为本轮完成边界，不另设真机验收待办。

自动化结果：`cargo test` 903 通过、0 失败、1 忽略；`pnpm test` 71 通过；`pnpm build`、`cargo check`
与定向仲裁/IPC 测试均通过。

## 13. 预估影响文件

- `src-tauri/src/daemon/popup_focus.rs`（新）：纯仲裁状态机。
- `src-tauri/src/daemon/unix_impl/mod.rs`：ServerState、dispatch、GUI 服务循环、终态/断连、托盘 focus 接线。
- `src-tauri/src/daemon/request.rs`：按 seq 暴露/统一 GUI surface 登记与移除辅助。
- `src-tauri/src/ipc/mod.rs`：ready/focused/dismissed/presentation 消息与模型。
- `src-tauri/src/app/mod.rs`：冷/热隐藏建窗、reader 消息、WindowEvent、统一展示。
- `src-tauri/src/commands.rs`：ready 命令替代 warm-only 直接 show。
- `src-tauri/src/macos_window_order.rs`（新）及模块声明：原生级联、非激活排序、window number。
- `src/views/popup/usePopupCore.ts`：冷/热统一在内容就绪后报告 ready。
- 对应 Rust/Vitest 测试。
- 实现完成后更新 `docs/overview-popup-ui.md`。

## 14. 风险与回退

- **跨进程 z-order 平台差异**：Phase 0 先验证；macOS relative ordering 不可用时退到 `orderBack`，不允许
  用“先抢焦点再抢回”伪装非激活显示。
- **ready/terminal 并发**：用纯状态机 + 幂等事件封住，所有副作用在解锁后执行。
- **等待窗口永久隐藏**：owner/前驱失败、socket EOF和 ready 超时都必须触发重新求值。
- **接力太早**：terminal 与 dismissed 分离，EOF和 750ms 超时兜底。
- **预热性能回归**：ready 握手是本机 daemon IPC，不等待 rAF；性能 harness 比较改造前后的 hot
  `cli.start -> gui.win_show/fe.painted`，出现明显回退则定位后再合入。
- **手动切换造成顺序困惑**：明确规则为“被选窗口成为 owner，旧 owner 回等待队首”，并由单测固定。

## 15. 建议提交粒度

1. `test(popup): cover concurrent focus arbitration`
2. `feat(popup): arbitrate focus across concurrent requests`
3. `feat(popup): cascade background windows on macOS`
4. `fix(popup): release focus ownership on every terminal path`（仅在确有独立修复时使用）
5. `docs(popup): document concurrent popup behavior`

每个功能/逻辑提交前均运行对应 Rust/前端测试；最终必须按项目要求执行 `./scripts/install.sh`，并用新安装的
AskHuman 完成真机验收。
