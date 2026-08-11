# 计划：本地 Markdown Mermaid 图表渲染

> 状态：**实现与本机自动验收完成，待跨平台实机补验**（方案于 2026-08-11 确认；
> 实现于 2026-08-12）
>
> 适用范围：AskHuman 的本地 WebView——Popup Message / Question、回复历史详情、Agent 控制台
> Watch / 完整会话。四个 IM 渠道及产品更新日志不在本计划内。
>
> 关联文档：`docs/overview-popup-ui.md`、`docs/specs/mcp.md`、
> `docs/specs/reply-history.md`、`docs/specs/gui-agent-console.md`、
> `docs/specs/popup-find.md`、`docs/specs/popup-launch-performance.md`。

## 1. 目标与非目标

### 1.1 目标

- 当 Agent 在 Markdown 中输出语言标记为 `mermaid` 的显式 fenced code block 时，在所有本地 Agent / AskHuman
  内容界面把它渲染为 SVG 图表。
- 普通 Markdown 保持现状；只有实际出现 Mermaid fence 时才加载 Mermaid，不增加普通 Popup 的
  关键路径下载、解析和首帧等待。
- 统一处理动态内容、主题切换、源码查看、代码复制、语法错误、超限、Popup Find 与无障碍语义，
  避免各界面分别操作 `v-html` 后的 DOM。
- 将 Mermaid 源文视为不可信输入；在 Tauri `withGlobalTauri: true`、`csp: null` 的现状下，
  禁止脚本、回调、可点击链接、HTML label 和外部资源加载。

### 1.2 非目标

- 不改变 CLI / MCP 入参、`AskRequest`、IPC、历史 JSONL 或 Agent transcript 数据结构；这些链路
  已经原样保存 Markdown 源文。
- 不在 Telegram、钉钉、飞书、Slack 中生成图片。四个渠道继续按平台原生 Markdown / mrkdwn
  显示 Mermaid 源码代码块；Daemon 端 headless 图片渲染是独立候选需求。
- 不为设置页 / Popup 更新提示中的 release notes 启用 Mermaid；它们不是 Agent 内容。
- 不自动猜测未标注的 Mermaid 文本，不渲染普通代码块，不引入在线渲染服务或 CDN。
- 不借此任务调整 Tauri 全局 CSP；图表渲染器自身必须在现有配置下闭合安全边界。

## 2. 当前实现与改造起点

- `src/lib/markdown.ts` 动态加载 `markdown-it`，配置为 `html: false`、`linkify: true`、
  `breaks: true`。所有 fence 当前统一包装为带复制按钮的普通 `.code-block`。
- Popup 的共享 Message / Question 分别在 `MessageSection.vue`、`SequentialPane.vue`、
  `QuestionCards.vue` 通过 `v-html` 消费 `renderMarkdown()`。
- `HistoryDetail.vue` 的 Message / Question、`WatchPane.vue` 的最近助手文字、
  `TranscriptPane.vue` 的 Assistant / AskHuman Message 走同一个字符串 renderer。
- `renderMarkdown()` 是同步纯字符串 API；它不知道 HTML 何时进入 DOM，也没有内容变更、主题切换、
  异步取消或卸载生命周期。Mermaid 不应直接塞进该函数并 fire-and-forget。
- 主题由 `src/lib/theme.ts::applyTheme()` 改根节点 class，system 模式另由
  `prefers-color-scheme` CSS 生效。Mermaid SVG 的颜色在生成时固化，必须显式重绘。
- Popup Find 会遍历 Text node 并插入 HTML `<mark>`；若进入 SVG 会破坏其结构，需增加原子
  Find 单元，而非继续沿用普通 DOM 标记。
- Vite 构建目标为 `es2019` + `safari13`，对应 macOS Catalina WKWebView 下限。构建转译通过
  不代表第三方库使用的 DOM API 在旧 WebView 可用。

## 3. 已确认决策

| 编号 | 决策 |
| --- | --- |
| M1 | 只识别 fence info 首词经 `trim + lower-case` 后等于 `mermaid` 的代码块；其它 info/普通代码块完全沿用现状。 |
| M2 | 使用完整 `mermaid` npm 包，不使用 CDN，也不使用官方 Tiny 包。Tiny 缺 Mindmap、Architecture、KaTeX 和 lazy loading，且官方不建议常规 npm 集成使用。具体版本在实施时选择当时最新、已含安全修复且通过旧 WebView gate 的版本。 |
| M3 | Mermaid 依赖必须位于独立动态 chunk；只有当前 Markdown 实际含 Mermaid fence 时才 `import("mermaid")`。普通 Popup 不发起该 chunk 请求。 |
| M4 | 新增共用 `MarkdownContent` 组件（名称可按实现微调），由它拥有 Markdown DOM、Mermaid 异步生命周期与事件委托；不在每个调用方复制 scan/render 逻辑。 |
| M5 | 覆盖 Popup Message / Question、History Message / Question、Agent Watch、完整会话 Assistant 与 AskHuman Message。release notes renderer 保持不变。 |
| M6 | `--no-markdown` 恒显示原文；Popup 整篇“查看源码”保持现状。图表成功后还须保留单图复制源码/查看源码能力，供 History 与 Console 使用。 |
| M7 | 加载和渲染是渐进增强：先显示可用的源码代码块，再异步替换为图；不得等待 Mermaid 才 show Popup 或打 `fe.painted`。 |
| M8 | 语法错误、未支持图型、依赖加载失败、旧 WebView 不兼容或资源超限时，保留原代码块并显示紧凑本地化提示；禁止 Mermaid 默认错误图污染正文。 |
| M9 | 首版单个 Markdown body 最多渲染 **10 张图**；单图源文最多 **40,000 字符**；全局 `maxTextSize=40,000`、`maxEdges=400`。超限图只回退源码，不影响其它图和回答交互。 |
| M10 | Mermaid 源文按不可信输入处理。优先使用 `securityLevel: "sandbox"`；sandbox API 返回的 data-URL iframe 不得原样插入，必须解码检查内部 SVG、重新清洗并用自有 CSP + 空 sandbox 权限重新封装。只有三平台 WebView 兼容 spike 证明 sandbox 不可用时，才可提议退到 `strict`，且必须先经 AskHuman 确认。绝不使用 `loose` / `antiscript`。 |
| M11 | `startOnLoad:false`；不调用 `bindFunctions`；禁用 HTML labels、click/callback 与外部资源。站点安全配置、主题、DOMPurify 配置、文本/边上限进入 `secure` 锁定集合，图内 frontmatter/directive 不得覆盖。 |
| M12 | light/dark/system 变化后从保存的源文重绘。使用 `theme:"base"` + 明确的十六进制 palette；不依赖现有 rgba / `color-mix` CSS token 被 Mermaid 主题引擎理解。 |
| M13 | 宽图先按容器做有限度自适应，保留 12px 可见字号下限；达到下限仍放不下时在图表容器横向滚动，不为塞进 Popup 宽度而无限缩小。容器宽度变化时重算，纵向仍由现有页面滚动容器承载，避免嵌套纵向滚动。 |
| M14 | Popup Find 把渲染后的图视为 `data-find-atomic` 原子单元：索引 SVG 的可见 label 文本，命中时高亮整张图并滚动定位，不向 SVG 插入 `<mark>`。源码模式仍按普通代码文本搜索和逐词高亮。 |
| M15 | 每次渲染使用唯一 DOM/SVG id；多个组件的 Mermaid 调用串行排队，避免全局配置与临时 DOM 互相踩踏。内容/主题变化采用 generation token，过期结果不得写回新 DOM。 |
| M16 | 不缓存带固定 id 的完整 SVG 字符串；若后续加缓存，必须解决重复 id、主题键和安全清洗版本键后另行设计。 |

## 4. Phase 0：依赖、兼容性与 Bundle Spike

在铺开组件改造前先做最小 spike；任一 gate 未通过时暂停并通过 AskHuman 对齐，不自行提高系统要求、
降低安全等级或缩减已确认图型范围。

1. 记录现有 `pnpm build` 与 `ANALYZE=1 pnpm build` 产物：入口 chunk、Popup 相关 chunk、
   `markdown-it` chunk、dist 总 JS（raw + gzip）及最终安装包体积。
2. 加入候选的完整 `mermaid` 版本和最小动态 import/render harness；确认 Vite 没把 Mermaid 或其图型
   依赖吸进 `index`、Popup 或 `markdown-it` 常用 chunk。
3. 用 flowchart、sequence、state、class、mindmap、CJK 与非法图验证：
   - `securityLevel:"sandbox"` 下能生成并正确自适应；
   - 不启用外链、callback、HTML label；
   - 多图渲染无 id 冲突；
   - `suppressErrorRendering`/等价处理不会遗留全局错误节点。
4. 验证 macOS WKWebView、Windows WebView2、Linux WebKitGTK。macOS 必须覆盖项目声明的
   Safari 13 / Catalina 级运行时；只有语法构建成功不算通过。
5. 若当时最新 Mermaid 无法运行于 Safari 13，先评估仍有安全修复的兼容分支；若仍失败，停止并
   让人类决定“放弃功能、限制图型/版本、或调整系统下限”，不得默认改变产品兼容承诺。
6. 把 spike 的版本、bundle 数字、三平台结果写入本计划的“实施记录”小节后再进入 Phase 1。

## 5. Phase 1：共用渲染基础设施

### 5.1 Markdown fence 标记

- 在 `src/lib/markdown.ts` 的 fence rule 中读取 token info：
  - 非 Mermaid：继续 `wrapCodeBlock()`，HTML 与复制行为不变；
  - Mermaid：仍输出已转义的 `<pre><code>` 和现有复制按钮，同时给 wrapper 增内部专用标记，
    供挂载后的组件发现。源文只放在 code text，不拼入未转义 attribute / HTML。
- 预加载 fallback（`markdown-it` 尚未 ready）继续是转义纯文本；renderer ready 后组件按现有响应式
  机制重算，再开始 Mermaid 动态加载。
- 增单测：info 归一、普通 fence 不变、源文/label 转义、复制按钮仍读原始 code text。

### 5.2 `src/lib/mermaid.ts`

新增单一 Mermaid adapter，隔离第三方 API 和版本差异：

- `loadMermaid()`：单例动态 import，失败结果可恢复重试但不形成高频循环。
- `renderMermaid(source, effectiveTheme, renderId)`：
  - 检查 40k/400 上限（边上限同时交给 Mermaid 配置执行）；
  - 按 effective theme 初始化固定配置；所有调用进串行队列；
  - `startOnLoad:false`、sandbox、`suppressErrorRendering:true`、HTML label/交互关闭；
  - 返回经过安全验证的静态结果，不返回可执行 bind function。
- sandbox 模式的返回值是包含 base64 HTML 的 data-URL iframe；adapter 必须按固定输出形态 fail-closed：
  1. 解析外层 iframe，只接受预期的 `data:text/html;charset=UTF-8;base64,...`，拒绝其它 URL / 元素；
  2. 解码内部 HTML，只接受单一静态 SVG 文档，提取 viewBox、`title/desc/text` 与安全 SVG；
  3. 移除 event handler、`<script>`、`<foreignObject>`、`<a>`、`<image>` 及非本地 fragment 的
     `href`/`xlink:href`/CSS `url(...)`；只保留 marker/clipPath 等内部 `#id` 引用；
  4. 把清洗后的 SVG 放入最小 HTML，注入 `default-src 'none'; style-src 'unsafe-inline';
     img-src 'none'; font-src 'none'; connect-src 'none'` 的 meta CSP，再编码为自己的 data URL；
  5. 由组件创建 `sandbox=""`（无 `allow-popups`、无 top-navigation、无 script/same-origin 权限）的
     iframe；不直接复用 Mermaid 返回的 sandbox attribute。
- adapter 返回 `{ documentUrl, intrinsicSize, findText, accTitle, accDescription }` 一类结构化结果；
  iframe 插入后不需要、也不允许父页面跨 origin 读取内部 DOM。任何输出形态变化、解码或清洗失败都
  整体回退源码。若未来经确认改用 `strict`，为裸 SVG 另设明确分支，不能混用 sandbox 假设。
- 配置的 `secure` 至少覆盖 `secure`、`securityLevel`、`startOnLoad`、`maxTextSize`、`maxEdges`、
  `theme`、`themeVariables`、`themeCSS`、`fontFamily`、`htmlLabels`、`dompurifyConfig`。
- 每次渲染用进程内单调序号 + 窗口随机前缀生成唯一 id；错误只返回结构化原因，不向正文或 console
  输出未经节流的大段 parser 内容。

### 5.3 `MarkdownContent` 组件

建议 props：`source`、Markdown copy labels、`enableMermaid`（Agent 内容为 true，默认 false 或显式传入）；
根元素透传 class、`data-find-seg` 与 click 事件，以兼容现有样式和事件委托。

组件生命周期：

1. 计算 `renderMarkdown(source)` 并写入自己的 root。
2. `nextTick` 后扫描 Mermaid wrappers；第 11 张起标为超限 fallback。
3. 每张图先保留源码块，进入 loading 状态；按 5.2 串行渲染。
4. 成功后在 wrapper 内加入 `.mermaid-canvas` 和无权限 sandbox iframe，原 `<pre><code>` 留作
   源码/复制来源但默认隐藏；
   失败时不移走 `<pre>`。
5. 用 generation token 检查 `source`、Markdown renderer ready、locale copy labels、主题 revision 与组件
   存活状态；任一变化使旧结果失效。
6. 卸载时取消待写回状态并清理组件创建的临时节点；不得清理 Mermaid/其它组件的全局 DOM。

组件内的图表工具条至少包含：复制源码、图/源码切换、失败/超限状态。按钮走既有复制图标和
`common.copyCode` / `common.copied` 文案；新增的 rendering/error/too-large/show-diagram/show-source
文案进入中英文 i18n。

## 6. Phase 2：主题、Find、布局与无障碍

### 6.1 有效主题信号

- `src/lib/theme.ts` 暴露只读 effective color scheme / revision：
  - `applyTheme(light|dark|system)` 更新模式并触发 revision；
  - system 模式注册单例 `matchMedia("(prefers-color-scheme: dark)")` listener，系统切换也触发；
  - listener 生命周期与当前窗口一致，不由每个 Markdown 组件重复注册。
- Mermaid palette 用固定十六进制值对齐现有明暗 token，至少配置背景推导、primary/secondary/tertiary、
  node text/border、line、note、font family；外层 canvas 保持透明以兼容 Solid/Blur/Glass。
- 主题变更从源文完整重绘，不尝试只用 CSS 覆盖已生成 SVG 的内嵌颜色。

### 6.2 Popup Find 原子节点

- 扩展 `src/lib/findInDom.ts`：普通 Text node 维持现有 `<mark>` 逻辑；遇到
  `[data-find-atomic]` 时不下降遍历，由 adapter 提供该原子单元的可见文本与命中目标。
- Mermaid 成功后在 sandbox 文档重新编码前，从已清洗 SVG 的 `<title>/<desc>/<text>` 提取可见
  label 文本，写入父页面的原子 Find 元数据；
  不索引 `<style>`、隐藏源码或内部 id。
- 同一张图内无论 query 出现几次，Rendered 模式按“一张图一个命中目标”计数并高亮 wrapper；
  Source 模式恢复逐次文本命中。这一规则避免同一个 HTMLElement 对应多个 Find index 的状态冲突。
- `usePopupFind.ts` 的 DOM-backed segment 重建必须认识原子目标；sequential 切题、vertical 滚动、
  Mermaid 异步完成、主题重绘后均重放当前查询和定位。
- 增回归测试：Find 打开时图异步完成、主题切换、源码/图切换、图 label 命中、普通段落 + 图混合、
  clear marks 后 SVG 字节结构未被 `<mark>` 改写。

### 6.3 布局与无障碍

- `.mermaid-block` 与 `.mermaid-canvas` 复用 Markdown 段落间距、圆角和轻量背景；宽图只在 canvas
  内横向滚动，工具条 sticky/右上角定位不能遮住图标题。
- sandbox iframe 依据清洗前提取的 viewBox/intrinsic size 定尺寸，内部 SVG 保持 viewBox + intrinsic
  width；禁止一律 `width:100%` 把超宽图压成缩略图，普通宽度可在容器内居中。
- 成功图的父 wrapper 使用 `figure`/`role="img"`，iframe 设置非空 `title`；优先消费 Mermaid
  `accTitle` / `accDescr`，否则用本地化 “Mermaid diagram”标签。源码按钮可键盘操作，focus ring
  与现有控件一致。
- `prefers-reduced-motion` 下关闭图表出现动画；本功能不主动执行 Mermaid 动画或交互。

## 7. Phase 3：接入全部本地 Agent 内容界面

按以下顺序替换直接 `v-html="renderMarkdown(...)"`；每完成一类就做组件测试，避免最后一次性定位回归。

1. **Popup**
   - `MessageSection.vue`：共享 Message；保留整篇 `viewSource`、复制 Message、`data-find-seg`。
   - `SequentialPane.vue` / `QuestionCards.vue`：单题与纵向多题题干；保留 transition、card refs、
     当前题逻辑和代码复制/链接事件委托。
   - Permission Confirm / update notes 不在本轮启用 Mermaid。
2. **History**
   - `HistoryDetail.vue` 的 Message / Question；条目切换后 generation token 必须阻止上一条 SVG 写回。
   - 保持只读、附件预览、链接外开和普通代码复制行为。
3. **Agent Console**
   - `WatchPane.vue`：frame 文本频繁刷新时，同源 frame 不重复渲染；新源使旧结果失效。
   - `TranscriptPane.vue`：Assistant 文本、AskHuman Message；向上分页插入旧事件后不打乱滚动锚点，
     Mermaid 异步变高时对“用户正在底部”与“正在读旧内容”分别保持合理位置。
   - 200 条分页可能同时出现多图；每个 Markdown body 的 10 图上限之外，再按可见/临近视口调度，
     避免一次进入完整会话就并发渲染整页所有图。首版可用 IntersectionObserver 启动渲染，
     已开始的单图仍走串行队列。

原型 `src/prototype/AgentConsoleProto.vue` 不作为生产入口；若仍用于视觉对照，可在生产组件稳定后再同步，
不能让原型阻塞验收。

## 8. 测试与验证

### 8.1 自动测试

- `src/lib/markdown.test.ts`：fence 识别、转义、普通代码块/复制不回归。
- `src/lib/mermaid.test.ts`（新增）：配置锁定、资源上限、唯一 id、队列、过期取消、错误归一、
  sandbox data URL 解码/重编码、自有 CSP 与空 sandbox 权限、输出清洗、外部引用拒绝。第三方
  renderer 用 mock，另保留少量真实 Mermaid integration test。
- `MarkdownContent` 组件测试：加载→成功、加载失败、语法错、图/源码切换、复制、locale/theme 更新、
  unmount race、10/40k/400 边界。
- `findInDom` / `usePopupFind`：原子图命中与 SVG 不被改写。
- Popup / History / Console 组件测试：各表面至少一张图；Transcript 分页和 Watch 快速 frame 回归。
- 安全 corpus 至少覆盖：HTML/event handler、`click ... callback`、外链 click、远程 image、CSS
  `url(https://...)`、frontmatter/directive 试图改 `securityLevel/theme/maxEdges`、畸形 SVG label。

### 8.2 构建与体积

1. `pnpm test`
2. `pnpm build`（TypeScript + Safari 13 target）
3. `ANALYZE=1 pnpm build`
4. 对比 Phase 0 基线并记录：
   - 无 Mermaid 时入口/Popup/markdown-it chunk 不含 Mermaid；
   - Mermaid 为独立 lazy chunks；
   - dist 与最终安装包 raw/gzip 增量；
   - 普通 Popup 的 `fe.bootstrap`、`fe.mounted`、`fe.painted` 不等待 Mermaid。

体积显著超出 spike 预期、常用 chunk 被污染或无 Mermaid 性能门回归时暂停评审，不能用提前加载换取
实现便利。

### 8.3 安装与人工验收

功能/逻辑完成后按仓库约定运行 `./scripts/install.sh`，后续提示使用新安装的 `AskHuman`。

- Popup：冷/预热、Message/单题/纵向多题、源码模式、`--no-markdown`、Find、附件与回答流程。
- 图型：flowchart、sequence、state、class、mindmap、CJK、多图、40k/400 边边界与 10 图上限。
- 错误：非法语法、未知图型、动态 import 失败、清洗拒绝均只回退当前图，不阻止作答。
- 主题：light/dark/system、系统运行时切换、Solid/Blur/Glass。
- History：打开不同条目、搜索后看详情、快速切换条目。
- Console：Watch 连续刷新、完整会话进入底部、向上分页、滚动中 SVG 变高不跳走。
- 平台：Catalina 级 WKWebView、当前 macOS、Windows WebView2、Linux WebKitGTK。
- IM 回归：四渠道仍收到原始 Mermaid fence 代码块，不新增图片、附件或网络渲染调用。

## 9. 文档与完成条件

实现完成后按实际行为更新：

- `docs/overview.md`：本地 Markdown renderer 增 Mermaid（不改变 CLI/Daemon 架构图）。
- `docs/overview-popup-ui.md`：支持表面、源码/失败/超限、主题与 Find 语义。
- `docs/specs/mcp.md`：MCP Message/Question 的本地 Mermaid 扩展与 IM 降级。
- `docs/specs/reply-history.md`：历史还原 Mermaid 图。
- `docs/specs/gui-agent-console.md`：Watch/Transcript Mermaid 与懒调度。
- `docs/specs/popup-find.md`：Rendered 模式图表原子命中规则。
- 如 bundle/perf 不变量需要长期守护，在 `docs/specs/popup-launch-performance.md` 增“无 fence 不加载
  Mermaid”的门；不要把一次 spike 数字写进主 overview。

完成条件：所有本地目标表面、主题、Find、安全、旧 WebView、无 Mermaid 性能门与安装实测全部通过；
随后删除 `docs/PROGRESS.md` 中对应待办 section。任一必需平台或安全 gate 未通过都不能把功能标为完成。

## 10. 实施记录

### 10.1 依赖、兼容与安全

- 采用完整 `mermaid@11.16.1`，通过独立 adapter 动态加载。Vite 的 Safari 13 target 保持不变；为
  Mermaid 依赖的 Marked 保留运行时 regexp lookbehind 探测，避免构建机把 feature probe 常量折叠成
  Catalina 无法解析的正则。渲染队列期间还补齐 Safari 13 缺少的 ES built-ins，并用原生
  `<style>.sheet` 兼容不可构造的 `CSSStyleSheet`。
- `pnpm audit --prod` 没有发现 Mermaid 引入的 advisory。审计仍报告基线已存在的两条传递依赖问题：
  `vue -> @vue/compiler-sfc -> postcss@8.5.18 -> nanoid@3.3.16` 的一条 high 和一条 moderate；同版本在
  加 Mermaid 前的 lockfile 已存在，本功能未扩大其依赖路径。
- 真实 Mermaid integration tests 覆盖 flowchart、sequence、state、class、mindmap、CJK、非法语法、
  directives 锁定、外链拒绝、唯一 id 与错误 DOM 清理。sandbox 输出只接受固定 base64 iframe 形态，
  解码后的单 SVG 经 fail-closed 元素 / attribute / CSS / 外部引用验证，再以自有 CSP 和空权限
  `sandbox=""` iframe 重封装。

### 10.2 Bundle

Phase 0 基线与最终 `ANALYZE=1 pnpm build`（均为 minified raw / gzip）：

| 产物 | 基线 | 最终 | 说明 |
|---|---:|---:|---|
| `index` | 118.32 / 35.41 kB | 123.53 / 37.24 kB | 常用入口增加 5.21 / 1.83 kB；不含 Mermaid core |
| `markdown-it` | 102.43 / 45.92 kB | 102.43 / 45.92 kB | 无变化 |
| `theme` | 241.01 / 86.48 kB | 242.06 / 86.91 kB | 主题信号与共用组件接线 |
| Mermaid adapter | — | 10.26 / 4.32 kB | 仅发现实际 Mermaid fence 后加载 |
| Mermaid core | — | 36.32 / 12.09 kB | 独立 lazy chunk；各图型继续按 Mermaid 自身动态拆分 |
| 全部 JS chunks | 697,591 / 234,738 B | 4,137,359 / 1,205,667 B | 包含所有不会同时下载的 Mermaid 图型 lazy chunks |

普通 Markdown 不触发 adapter import；完整会话还以 320px root margin 的 IntersectionObserver 调度
近视口 body。宽图、主题重绘、源码切换、10 图 / 40k 字符限制和无 fence 首屏门均在本地浏览器
harness 验证。

### 10.3 自动与本机运行验证

- `pnpm test`：26 个文件、160 个测试通过，包含 Popup、History、Console Watch / Transcript 的
  表面接线回归、实际 Markdown 正文字号传递，以及有限度自适应的缩放下限 / 容器 resize。
- `pnpm build` 与 `ANALYZE=1 pnpm build`：均通过，转换 2,317 个模块；大于 500kB 的警告来自 Mermaid
  的按图型 lazy chunk，不进入普通 Popup 首屏。
- `./scripts/install.sh`：通过，前端已嵌入 local-install 二进制并完成正式签名；最终安装二进制
  38,845,344 bytes。Phase 0 未保留同 profile 的安装前二进制，因此不虚构二进制增量，bundle 增量以
  §10.2 的可复现数字为准。
- 本地 Chromium WebView harness 实测 6 张有效图（五种必需图型加超宽图）和一张非法图：有效图均
  使用空 sandbox、自有 data document；非法图保留源码；图 / 源码切换会切换 Find 原子语义；超宽图
  横向滚动；dark 切换重绘且无 console error / warning。
- 当前 macOS Tauri Popup 实测 Message、Question、宽图滚动、图 / 源码切换与非法语法回退均正常。
  首轮人类验收指出图中文字偏大，随后改为读取各表面 Markdown 容器的实际 `font-size` 并传给 Mermaid
  主题，避免 Popup、History 与 Console 使用脱离正文的固定字号。第二轮反馈选择“有限度自适应”：
  保留 Agent 指定的 `LR/TD` 布局方向，宽图优先缩到容器，但不让可见文字低于 12px，超出部分继续
  横向滚动；第三轮当前 macOS Tauri Popup 复核确认效果符合预期。
- Safari 13 缺失 built-ins 与不可构造 CSSStyleSheet 由 integration tests 模拟通过；当前 macOS Tauri
  Popup、Catalina 级 WKWebView、Windows WebView2 与 Linux WebKitGTK 的真实运行验收仍需分别补做。

文档边界复核后，仓库级架构地图和不变量未改变，因此未修改主 `docs/overview.md`；实现行为记录在
Popup 专题 overview 与对应功能 specs。
