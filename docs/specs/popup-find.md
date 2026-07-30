# 需求：弹窗页内查找（⌘F / Ctrl+F）

> 状态：已实现（待真机验收）
> 关联现状：`docs/overview-popup-ui.md`、`src/views/popup/*`、`src/views/settings/useSearch.ts`（范式对照，非复用）
> 关联计划：`docs/plans/popup-find.md`

## 1. 背景

AskHuman 弹窗常承载很长的共享 Message（Markdown）与题干。用户需要在正文中快速定位关键字，但：

- 自定义 Tauri WebView 窗口没有可用的浏览器原生「页内查找」；
- 设置页 ⌘F 是**静态索引跳转**，历史是**列表关键字过滤**，都不适合「当前这篇长文里找字」。

因此在弹窗内提供浏览器式 **页内 Find**。

## 2. 目标与非目标

### 2.1 目标

- 支持 **⌘F（macOS）/ Ctrl+F（Windows/Linux）** 打开查找。
- 在问题侧只读内容中做**连续子串**匹配，高亮全部命中，支持上/下一条与匹配计数。
- 覆盖普通问答与 Confirm；顺序多题可**跨题**匹配并自动切题定位。
- 交互对齐常见浏览器 Find，且不破坏现有提交 / 取消 / 选项 / 语音快捷键。

### 2.2 非目标

- 不搜用户答案草稿（textarea）、不搜历史 / 设置 / Agent 控制台等其它窗口。
- 不做正则、整词、替换、多关键字 AND。
- 不改后端协议、结果区块、历史存储或 daemon。
- 不做跨弹窗 / 全局搜索。
- 第一版不在 navbar 放常驻放大镜入口（仅快捷键唤起浮动条）。

## 3. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| F1 | 交互范式 | 浏览器式页内 Find 条（高亮 + 上/下一条），不是设置页索引列表 |
| F2 | 搜索范围 | Message + 题干 + **预设选项** + **附件名** + Confirm 详情（及 choice 文案） |
| F3 | 不搜 | 用户 textarea / 备注草稿、页脚与导航 chrome 文案 |
| F4 | 匹配规则 | 连续子串；默认**不区分大小写**；查找条提供 **Aa** 开关 |
| F5 | UI 位置 | **导航栏右侧操作区**叠放：打开时动作按钮渐隐，查找条自上方滑入；关闭反向；无 focus ring |
| F6 | 多题 | 匹配全集按阅读序；顺序模式命中隐藏题时 **自动切题** 再定位 |
| F7 | 快捷键 | 见 §5；Esc **直接关闭**并清高亮；⌘G / Ctrl+G **仅查找条打开时**有效 |
| F8 | 选区预填 | 打开时若内容区有选区，预填查询并立即匹配 |
| F9 | 循环 | 到最后一条再「下一条」回到第一条（上一条对称） |
| F10 | 会话生命周期 | 关闭查找条 = 结束会话（清查询、高亮、索引）；不再保留无条 ⌘G |
| F11 | Confirm | 与普通问答**共用**同一套 Find |
| F12 | 源码模式 | Message 处于「查看源码」时，按**当前展示**文本搜索（源码搜源码，渲染搜渲染纯文本） |

## 4. 范围与匹配序

### 4.1 阅读序（匹配索引顺序）

普通问答：

1. 共享 Message（当前展示：渲染 HTML 的文本节点，或源码 / plain `pre` 全文）
2. AI→人附件的**文件名**（`.att-name`）
3. 按题号 `1…n`：
   - 题干文本
   - 该题预设选项文案（按选项顺序）

Confirm：

1. detail 正文（Markdown 渲染后的纯文本，或等价展示）
2. 各 choice / 行标签文案（按展示行顺序）

用户输入区、ComposerDock、Footer、Navbar **不入索引**。

### 4.2 顺序多题（非纵向）

- 逻辑匹配基于请求模型中的题干/选项字符串 + 当前 Message DOM，**不依赖**隐藏题是否已挂载。
- 当「当前命中」落在非当前题：先 `goNext` / `goPrev`（或直接设题索引）切到该题，`nextTick` 后对挂载 DOM 打高亮并 `scrollIntoView`。
- 查找条计数为**全局** `current/total`，不是「本题内」。

### 4.3 纵向多题

- 全部题卡在 DOM 中：直接在对应节点高亮并滚动即可；仍维护同一全局索引序。

## 5. 交互与快捷键

### 5.1 查找条 chrome

浮动条（建议结构）：

```
[ 🔍  输入框………………… ]  3/12  [▲] [▼]  [Aa]  [✕]
```

- 输入即时过滤（debounce 可选，建议 ≤100ms 或不 debounce，正文量级可接受）。
- `3/12`：有查询且 `total>0`；`0/0` 表示无匹配（输入框可用轻量错误态边框，不弹窗）。
- ▲ / ▼：上一条 / 下一条（与快捷键同效）。
- Aa：切换区分大小写；切换后保持当前相对位置策略见 §5.3。
- ✕ / Esc：关闭条并清高亮。

### 5.2 快捷键

| 操作 | macOS | Windows / Linux | 备注 |
|---|---|---|---|
| 打开或聚焦查找条 | ⌘F | Ctrl+F | 无 Alt/Shift；`preventDefault` |
| 下一条 | Enter（焦点在查找输入）或 ⌘G | Enter 或 Ctrl+G | 仅条打开时 |
| 上一条 | ⇧Enter 或 ⌘⇧G | ⇧Enter 或 Ctrl⇧G | 仅条打开时 |
| 关闭 | Esc | Esc | 直接关闭，不清「先清空再关」 |

焦点在答案 textarea 时按 ⌘F：打开条并把焦点**移到查找输入**（浏览器行为）。

### 5.3 打开 / 关闭 / 导航

**打开**

1. 记录关闭后可恢复的 `document.activeElement`（可选弱引用）。
2. 若窗口选区落在可搜索内容内且非折叠空选区：将选中文本写入查询（可截断极端长选区，如 200 字符）并立即匹配。
3. 显示条、聚焦查找输入；若预填则全选输入框文本便于改写。
4. 已打开时再按 ⌘F：仅聚焦查找输入（不重置查询）。

**导航**

- `total === 0`：上下一条 no-op。
- 否则循环：`current = (current ± 1 + total) % total`（1-based 展示）。
- 每条切换：滚动到当前命中（`block: "center"` 优先，避免被底部 ComposerDock 挡住时可再微调 offset）。
- 非当前高亮用一种样式，当前命中用更强样式。

**Aa 切换**

- 重新计算匹配集；尽量保持「同一文本偏移」附近的命中为当前，找不到则回到第一条或 0。

**关闭**

- 隐藏条；清空 query、current、total、高亮、DOM mark；结束会话。
- 尝试将焦点还给打开前控件（仍在 DOM 且可见时）；失败则不强制 focus 答案框。

### 5.4 与现有快捷键共存

- ⌘F / Ctrl+F 在 `onKeydown` 中**优先**处理（建议 capture 或置于业务键之前）。
- 查找条打开时：
  - 焦点在查找输入：字母与编辑键归输入框；Esc 关条。
  - 录音中的 Esc：若条打开则**先关条**，不在同一次按键停语音。
- ⌘↵ 提交、⌘W 取消、⌘1–9 选项、⌘[ / ⌘]、语音快捷键：查找条打开时**仍可用**（焦点不在查找输入、或即使在查找输入也可保留 mod 组合——推荐：**带 ⌘/Ctrl 的业务快捷键在条打开时仍生效**，与浏览器「Find 打开时仍可 ⌘W」一致；纯字符仅影响查找输入）。
- 不把 `f` 加入 `shortcutConflict` 禁区以外的特殊项；Find 为内置固定快捷键，用户自定义语音快捷键若录成 ⌘F 则冲突——实现时在 `shortcutConflict` 增加对 `f` 的拒绝，或录制时提示（推荐扩展 conflict：`cmd/ctrl+f` 保留给查找）。

## 6. 高亮与实现要点

### 6.1 模块边界（建议）

| 模块 | 职责 |
|---|---|
| `src/lib/findInDom.ts`（纯函数） | 在文本中找全部 range；可选 TreeWalker 包装 / 清除 mark |
| `src/views/popup/usePopupFind.ts` | 打开态、query、case、current/total、跨题导航、快捷键、与 request 模式协作 |
| `src/views/popup/FindBar.vue` | 浮动条 UI |
| `popup.css` + tokens | 高亮与条样式（亮/暗主题） |
| i18n `popup.find.*` | placeholder、Aa/上下条/关闭 title、无障碍 label |

`usePopupCore` / `context`：挂载 find API，在现有 `onKeydown` 中接入；避免把高亮逻辑写进 MessageSection 模板。

### 6.2 高亮策略

**推荐：在可搜索根节点内对文本节点包装 `<mark class="popup-find-hit">`，当前项加 `popup-find-hit-current`。**

- Markdown 已是 `v-html`：每次 `messageHtml` / 题干 HTML 重渲染后必须 **重放** 高亮（`watch` query + html + viewSource + currentQ）。
- 包装时跳过 `script`/`style`；尽量不拆开输入类控件（范围内本无）。
- 清除：关条或重算前 `unwrap` 全部 find mark，避免残留破坏复制/选区。
- 备选：CSS Custom Highlight API（不改 DOM）；WKWebView/WebView2 支持度需验收，可作为优化而非第一依赖。

**安全**：只包装文本节点，不把查询串当 HTML 插入。

### 6.3 滚动与固定编辑器

- 命中在 Message 上方时，用户可能处于 ComposerDock 固定态——Find 滚动**不**清除 composer owner（与「点 Message 选字」一致）。
- `scrollIntoView` 目标为当前 mark；若被底部 dock 遮挡，使用 `scroll-margin-bottom` 或手动计算 `.content` 内 scrollTop。

### 6.4 性能

- 单次弹窗正文通常 < 几百 KB；全量扫描可接受。
- 若实测输入卡顿：对 query 输入 50–100ms debounce；导航本身不 debounce。

## 7. 视觉

- 浮动条：毛玻璃/实底与现有 popup 控件一致，圆角、阴影，右上角 `position: sticky` 于 `.content` 或 `absolute` 相对 `.popup`（需保证随窗口缩放不错位，且不挡住拖拽区误触——条在 content 内更佳）。
- 非当前命中：柔和黄/琥珀底；当前命中：更深或描边（暗色主题提高对比）。
- 与历史列表 `<mark>`、设置 `search-hit-highlight` **样式隔离**（独立 class 前缀 `popup-find-*`）。

## 8. 无障碍

- 查找输入：`role="searchbox"` 或 `type="search"` + `aria-keyshortcuts`。
- 匹配计数：`aria-live="polite"` 播报 `3 of 12` / 本地化。
- 按钮具备 `aria-label`（上一条/下一条/区分大小写/关闭）。

## 9. 测试计划

### 9.1 单测（纯逻辑）

- 子串匹配：大小写开关、空查询、Unicode、重叠边界。
- 阅读序索引构建（fixture 请求模型）。
- 循环导航取模。

### 9.2 组件 / 集成

- ⌘F 打开条；再 ⌘F 聚焦；Esc 关闭清 mark。
- 预填选区。
- Message Markdown 与 plain / 源码模式切换后高亮一致。
- 选项文案、附件名可命中。
- 顺序多题：命中 Q2 时自动切题。
- Confirm detail + choice。
- 与 ⌘↵ / ⌘W 共存（条打开时仍可提交/取消）。

### 9.3 真机

- macOS / Windows / Linux 各测一遍快捷键。
- 长 Markdown + 代码块内查找。
- 纵向多题 + docked composer 时当前命中仍完整可见。

## 10. 文档与收尾

- 实现完成后：本 spec 状态改为「已实现」；按需补 `docs/plans/popup-find.md` 与 `docs/overview-popup-ui.md` 一小段。
- 用户 wiki：若已有快捷键说明页，补充弹窗查找；无则可不单开。

## 11. 实现分期建议

| 阶段 | 内容 |
|---|---|
| P0 | Find 条 + Message/plain/源码 + 快捷键 + 高亮/循环/Aa/选区预填 |
| P1 | 题干、选项、附件名、跨题（顺序/纵向） |
| P2 | Confirm 共用 |

P0–P2 可同 PR 若体量可控；否则按上表拆，但快捷键与条在 P0 一次到位，避免半成品入口。
