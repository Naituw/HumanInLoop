# 开发计划：弹窗输入框回看时固定在底部

> 关联需求：`docs/specs/popup-pinned-composer.md`
> 状态：已实施并通过真机验收。仅改 Popup 前端与对应测试 / 文档，不改 Rust 数据模型和提交协议。

## 0. 当前实现起点

- `PopupView.vue`：`.popup` 是纵向 flex；`.content` 是唯一纵向滚动容器；`PopupFooter` 在滚动容器外。
- `QuestionCards.vue`：纵向多题直接内联 textarea、语音 / 图片按钮、语音状态、图片缩略和回复文件。
- `SequentialPane.vue`：单题与旧版顺序多题有一份几乎同构的输入模板。
- `usePopupCore.ts`：
  - `inputByQ/imagesByQ/replyFilesByQ` 按题保存答案；
  - `inputRefs`、`focusedQ` 管理 textarea；
  - `current` 同时是导航 / 快捷键 / 语音默认目标；
  - `.content @scroll` 更新 `scrolled/atTop`，纵向模式经 rAF 跑 scroll-spy；
  - `autoGrow` 内联态封顶 240px；纵向空输入失焦后折回一行。
- 当前无独立 composer owner，也没有可承载 Teleport 的固定区 target。

## 1. 抽出共享 `AnswerComposer.vue`

新建 `src/views/popup/AnswerComposer.vue`，由 `QuestionCards.vue` 和 `SequentialPane.vue` 共用。

### 1.1 输入

- `qIndex: number`
- `collapsed: boolean`（仅纵向多题使用）
- 其余状态与动作继续通过 `usePopupContext()` 获取，避免把现有上下文拆成大量 props / emits。

### 1.2 组件内容

- source anchor（始终留在题内，登记 DOM ref）；
- `Teleport` 包裹的 `.answer-composer`：
  - textarea；
  - mic / image 按钮；
  - 当前题语音错误 / 状态；
  - 图片缩略和回复文件；
- `Teleport :disabled="dockedComposerQ !== qIndex"`；启用时 target 指向 Popup 根部唯一 dock slot。

### 1.3 既有模板迁移

- 从 `QuestionCards.vue` 和 `SequentialPane.vue` 删除重复输入区，改为 `<AnswerComposer>`。
- 保留原 v-model 数据、按钮行为、图片 / 文件删除行为和样式语义。
- `setInputRef` 在挂载和卸载时都写入（包括 `null`），避免顺序模式 Transition 后保留旧 DOM 引用。

## 2. Popup 根布局增加 dock shell

在 `PopupView.vue` 的 `.content` 与隐藏 file input / `PopupFooter` 之间增加常驻 target：

- shell 始终存在，未固定时隐藏且不占高度；
- 固定时作为 `flex: 0 0 auto` 子项参与布局，不覆盖 `.content`；
- 多题显示 `Question i/n` + 返回原题按钮，单题省略题号；
- target 承载 Teleport 过来的 `.answer-composer`；
- shell 只在普通 ask、非 select-only 且 `dockedComposerQ != null` 时可见。

建议拆成轻量 `ComposerDock.vue`，由它负责题号、返回按钮、region / aria-label；编辑器内容仍由
`AnswerComposer.vue` Teleport 进入其 slot target。

## 3. 状态与 DOM 引用

在 `usePopupCore.ts` 增加：

- `composerOwnerQ = ref<number | null>(null)`
- `dockedComposerQ = ref<number | null>(null)`
- `ownerSeenInline = ref(false)`
- `ownerManuallyActivated = ref(false)`
- `ownerScrolledUpAfterActivation = ref(false)`
- `composerAnchorRefs[]`
- `composerInlineHeights[]`
- `composerHomeHeights[]`（只记录 textarea / input-wrap 原位高度，供固定 / 回位判定）
- `composerResizeObservers[]` 或一个共享 `ResizeObserver`
- DOM 移动期间的焦点快照 `{ qIndex, hadFocus, selectionStart, selectionEnd }`

新增动作：

- `setComposerAnchorRef(el, qIndex)`
- `activateComposer(qIndex)`
- `measureComposerDock()`
- `returnComposerHome()`
- `resetComposerDock()`

`onTextareaFocus(i)` 不把裸 focus 直接视为手动；程序化 focus 显式传递自动 / 用户导航来源，然后调用
`activateComposer(i, manuallyActivated)` 并沿用现有 `focusedQ/setActive/autoGrow`。mousedown、click、keydown、
input 和显式用户导航才算手动激活；`onTextareaBlur` 只清
`focusedQ`，不清 owner / dock。初始顺序面板的 Transition `after-enter` 与请求上屏后的二次 focus 都必须
显式标成自动；旧版顺序多题由用户切题后的 `after-enter` 才标成手动导航。

初始自动 focus 前用 textarea home 与 `.content` 的交集计算可见比例；达到 50% 才 focus。不足 50% 时
不创建 owner，之后滚入视口也不补做自动 focus；用户切题导航不受此初始门槛限制。

请求初始化、提交、取消、窗口卸载和顺序模式题目卸载时清理 ref / observer / dock 状态。

## 4. 提取可测试的几何判定

新建 `src/views/popup/composerDock.ts`，放不依赖 Vue / DOM 的纯函数：

```ts
type DockGeometry = {
  homeTop: number;
  homeBottom: number;
  dockedHomeHeight: number;
  viewportTop: number;
  viewportBottom: number;
  viewportBottomAfterUndock: number;
};

resolveComposerDocked(currentlyDocked, ownerCanDock, geometry, hysteresis): boolean
```

规则：

- 首次固定时 textarea 已失焦 → 不固定；已经固定后的 blur 不撤销固定；
- 激活后尚未发生向上滚动 → 不固定；点击一个已被底边裁切的输入框不会立即固定；
- 既未手动激活、也未曾完整内联可见 → 不固定；
- 未固定：`homeTop >= viewportTop` 且
  `homeTop + dockedHomeHeight > viewportBottom` → 固定；高输入框先被底边逐步裁切到固定态可见高度；
- 已固定：把撤掉 dock 后 `.content` 会释放的高度计入 `viewportBottomAfterUndock`；同一条
  `homeTop + dockedHomeHeight` 投影底边位于 `viewportBottomAfterUndock - returnGap` 之上 → 回位，
  其余内联高度随继续滚动逐步露出；否则保持；
- home marker 在 viewport 顶部之上时不得从未固定态进入固定态。

实际常量先取 `RETURN_GAP_PX = 10`，实现后按真机边界观感微调；固定 / 回位必须使用不同阈值。

## 5. 滚动与布局同步

- 重构现有 scroll rAF：一次回调内分别更新纵向 `current` 与 composer dock 判定；单题 / 顺序模式虽不跑
  scroll-spy，仍跑 composer 测量。
- `onScroll` 只写轻量滚动状态并调度 rAF，不直接多次 `getBoundingClientRect()`。
- 固定 / 回位改变 flex 高度后，再调度下一帧测量，保证布局收敛。
- `autoGrow` 的增长路径只读 live `scrollHeight` 并在目标高度确实增加时单次写入；不会改变高度的
  普通字符不产生 style mutation。删除 / undo / 替换等缩短路径用 fixed、不可见的镜像 textarea
  测量后单次写回，禁止让聚焦中的 live textarea 临时经过 `height:auto`，避免 WebKit 滚动锚定抖动。
- 三种问题模式的 focus ring 统一由 `.input-wrap::after` + `:focus-within` 绘制；textarea 自身聚焦时
  只保留普通控件阴影。避免 macOS WebKit 在可滚动 textarea 单次增高后只刷新旧顶边与新底边的阴影
  脏区，造成 ring 侧边中段缺失；不得用二次 reflow 强制重绘来换回完整 ring。
- owner 刚激活时 `nextTick` 测量；只有 textarea home 至少一次完整落入 `.content`，才置
  `ownerSeenInline=true`。owner 激活后发生 `scrollTop` 减小才置 `ownerScrolledUpAfterActivation=true`；
  这个减小还必须紧跟向上的 wheel / trackpad 意图，防止预热窗口复用旧滚动位置时的钳制或 scroll
  anchoring 被误判。用户手动激活只提供“不必先完整显示”的资格，仍须等激活后的向上滚动才首次固定。
  如此首帧 resize / footer / 引导区等被动布局变化，以及点击已被裁切的输入框，都不会让输入框自行固定。
- editor 回位后发生新的 mousedown / click / keydown / input 时，重置 `ownerScrolledUpAfterActivation` 和
  滚动基线；mousedown 必须先于可能在 focus 与 click 之间执行的 rAF，防止上一次固定周期的向上滚动
  资格在 click handler 重置前泄漏。
- 程序化导航先聚焦、再滚动的既有双 `nextTick` 流程中，不得把尚未出现的目标题误判为固定。

## 6. Placeholder 与 ResizeObserver

- `.composer-anchor` 在内联态由真实编辑器撑高；共享 `ResizeObserver` 记录每题最新完整编辑器高度，
  同时单独记录 textarea / input-wrap 的原位高度。
- 固定时 anchor 设 `min-height/height` 为最后一次内联高度，保持题卡与整个 `.content.scrollHeight` 稳定。
- 固定 / 回位几何把内联 textarea 高度按固定态 120px 上限投影，在保留多题 action row 等固定部分后，
  使用 `anchor.top + dockedHomeHeight` 作为连续切换边界；不等待下方图片 / 文件进入视口。
- observer 在 fixed compact 样式下不更新内联高度，避免 120px 上限 / 单行附件摘要污染 placeholder。
- 回位后解除固定高度，下一帧调用 `autoGrow(qIndex)` 并重新让 observer 接管。

## 7. Teleport、焦点与选区

在每次改变 `dockedComposerQ` 前：

1. 找到 owner textarea；
2. 记录它是否为 `document.activeElement`；
3. 若有焦点，记录 selection start / end；
4. 改 dock 状态，让 Teleport 移动同一节点；
5. `nextTick` 后仅在 `hadFocus=true` 时 `focus({ preventScroll:true })` 并恢复 selection。

若用户先点击 Message 导致 blur，`hadFocus=false`，固定 / 回位不得抢回焦点。返回原题按钮是用户明确动作，
滚动 / 回位完成后主动聚焦并恢复最后选区。

输入法验证重点放在 WebKit / WebView2：组合输入进行中若滚动触发 Teleport，应保证 composition 不提交两次、
不丢未完成文本。若某平台移动节点必然中断 composition，实现时在 `compositionstart/end` 维护标记，组合期
延迟 dock 切换到 `compositionend`。

## 8. 固定区紧凑样式

在 `popup.css` 增加：

- `.composer-dock`：上边框、实体 / 毛玻璃兼容背景、轻阴影、紧凑 padding；
- `.composer-dock-header`：多题题号 + 返回按钮；
- `.answer-composer.is-docked .textarea`：`max-height: 120px`；为空且 blur 也不折叠；
- 固定态 `.thumbs/.reply-files`：单行、横向 overflow、较小缩略图 / chip，保留删除按钮；
- source placeholder 的固定高度样式。

固定区出现后不做位移动画，避免输入中的 DOM 节点动画造成光标抖动；可只对边框 / 阴影做极短 opacity
过渡。`prefers-reduced-motion` 下全部即时。

## 9. 题目归属与既有动作

- `AnswerComposer` 中 mic / image 按钮先 `setActive(qIndex, false)`，再执行 `toggleSpeech/pickFiles`，确保
  owner 与 scroll-spy current 不一致时仍写对题。
- `toggle(qIndex, option)` 若 `qIndex !== composerOwnerQ`，先清除旧 owner / dock，再写入新题选项；这代表
  用户已明确切换作答上下文。只滚动、点题干或选择 Message 文本不清除 owner。
- 粘贴沿用 `focusedQ ?? current`；固定 textarea 重新获得焦点后 `focusedQ=owner`，自然归属正确。
- 拖放仍按物理落点题卡；固定区上的拖放若支持，则显式归 owner，否则保持现有 `.content` 落点规则。
- 点击 dock 题号：为纵向模式设置 current / 导航锁，滚动 anchor 到内容区内；顺序模式 owner 本就是当前题。
- `expandedQ(i)` 在 `dockedComposerQ===i` 时恒为 true。

## 10. i18n 与可访问性

新增中英文文案（具体 key 以实现为准）：

- `popup.composer.dockedLabel`：固定输入区 aria-label；
- `popup.composer.returnToQuestion`：返回原题按钮 title / aria-label。

题号正文复用 `popup.question.indexed`。固定区用 `role="region"`；不使用持续 `aria-live`，避免每次滚动
都打断屏幕阅读器，只在状态首次切换时依赖 region / focus 语义。

## 11. 测试

### 11.1 Vitest 纯函数

- owner 未内联可见不固定；
- 短输入框从底部越界固定；高输入框逐步裁切到固定态投影高度后固定；
- anchor 从顶部离开不固定；
- 已固定时投影边界尚未回归则保持；固定态投影高度回归 + gap 后连续回位；
- 附件区高于视口时仍按 textarea home marker 正常回位；
- 阈值附近滞回不振荡；
- 小视口 / 高 composer 的边界，以及 `240px ↔ 120px` 双向切换连续性。
- 连续普通输入不写高度；换行增长只有一次 live height mutation；缩短测量不把 live height 写成
  `auto`，镜像节点测量后立即移除。

### 11.2 Vue 组件测试

- Teleport 前后拿到的是同一个 `HTMLTextAreaElement` 对象；
- v-model 文本、selection 快照和焦点恢复路径不创建第二份 textarea；
- blur 后 dock shell 仍存在；聚焦另一题切换；
- 勾选另一题选项清除旧 owner，滚动 / 点正文不清除；
- 固定态多题标签与返回动作正确；
- compact 附件行保留删除动作。

### 11.3 构建与手工矩阵

- `pnpm test`
- `pnpm build`
- `cargo test --manifest-path src-tauri/Cargo.toml`
- `./scripts/install.sh`
- 手工：
  - 单题 + 长 Message；
  - 旧版顺序多题切题；
  - 纵向多题 owner 与 current 分离；
  - 空 / 多行 / 超 5 行输入；
  - Message 文本选择、复制后继续编辑；
  - 图片 / 文件、语音、粘贴、undo / redo、中文输入法组合；
  - resize、IM tip 显隐、浅色 / 深色、reduced motion；
  - macOS WebKit、Windows WebView2，Linux 条件允许时回归。

## 12. 文档收尾

实现完成后：

- 把 spec 状态改为“已实现”；
- 在 `docs/overview-popup-ui.md` 增加“固定答案编辑器”当前实现地图与 spec / plan 链接；
- 检查多问题纵向 / 交互重构文档中输入框折叠与焦点描述，只有被本需求改变的局部才补充覆盖关系；
- 主 `docs/overview.md` 的仓库级模块地图和不变量不变，不更新。

## 13. 实施顺序

1. 纯几何 helper + 单测。
2. 抽 `AnswerComposer.vue`，确保三种模式在未启用 dock 状态下行为无回归。
3. Popup dock shell / i18n / 紧凑样式。
4. owner / anchor / ResizeObserver / scroll rAF 状态接线。
5. Teleport + placeholder + 焦点 / 选区恢复；补 composition 保护。
6. 返回原题、多题归属、语音 / 附件动作校准。
7. Vue 组件测试、前端 / Rust 全量测试、安装与三模式真机验收。
