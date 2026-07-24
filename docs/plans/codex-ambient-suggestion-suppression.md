# Codex Ambient Suggestions 后台线程的 AskHuman 静默计划

> 状态：已实现；真实桌面端 Suggested prompts 验证按用户要求记录为项目待办，稍后执行。
> 范围：Codex 桌面版 Suggested prompts / ambient suggestions 与 AskHuman 全局 Rules、MCP 工具的交互边界。
> 当前验证基线：ChatGPT.app 26.715.52143，内置 `codex-cli 0.145.0-alpha.18`。
>
> **2026-07-24 修订**：ChatGPT.app 26.721 把 ambient 线程标签从 `thread_source=system` 改为
> `ambient_suggestions`，本计划「只匹配精确值 `system`」的守卫因此 fail-open。拦截集合已扩为
> `{system, ambient_suggestions}`（MCP guard 与 Stop 确认共用），并对未拦截的 Codex 调用增加
> `action="passed"` 放行审计；现行契约见 `docs/specs/mcp.md` 与 `docs/specs/agent-stop-confirmation.md`。

## 1. 问题与已验证事实

Codex 桌面版打开 “Suggest what to do next by searching project files and connected apps” 后，会为当前
项目自动启动一个生成首页建议的后台 thread。它不是用户主动创建的任务，也不是 Codex subagent，但当前仍会
加载项目适用的 `AGENTS.md` 并暴露 AskHuman MCP，因此可能执行 AskHuman Rules 里的强制提问和
`whats_next` 交接，导致用户尚未开始任务就收到弹窗或 IM 提问。

当前桌面版的实际启动参数已经确认：

| 参数 | Ambient suggestion thread | 普通用户 thread |
| --- | --- | --- |
| `threadSource` | `system` | 默认 `user` |
| `ephemeral` | `true` | 通常为 `false` |
| 触发方式 | 应用启动、返回或后台刷新 | 用户主动创建 |
| permission | 强制 `:read-only` | 用户当前选择 |
| approval policy | 强制 `never` | 用户当前选择 |
| 最终输出 | 0–3 条建议的 JSON schema | 普通对话输出 |

只读权限不会禁止 MCP 或本地 AskHuman CLI 联系用户。Ambient 生成器会关闭不允许用于个性化的 apps，
并关闭 browser/chrome/computer-use 一类本地插件提供的 MCP，但不会关闭用户自己安装的 AskHuman MCP；
它也没有给主生成 thread 设置 `project_doc_max_bytes = 0`。同一功能的安全分类 thread 已经使用
`project_doc_max_bytes = 0` 并关闭多类工具，说明上游具备隔离能力，只是主建议生成器尚未采用。

Codex 当前会在自定义 MCP 的每次 `tools/call` 请求 `_meta` 中发送：

```json
{
  "threadId": "…",
  "x-codex-turn-metadata": {
    "thread_source": "system",
    "thread_id": "…",
    "turn_id": "…",
    "sandbox": "read-only"
  }
}
```

普通用户任务的 `thread_source` 为 `user`。AskHuman 当前使用的 `rmcp` 已支持在 tool handler 中读取
`RequestContext<RoleServer>.meta`，但 `src-tauri/src/mcp/ask.rs` 尚未消费该字段。

## 2. 目标

1. Codex ambient/suggested-prompts 后台 thread 不得弹出 AskHuman、发送 IM 卡片或写入项目 todo。
2. 用户点击建议并真正启动任务后，AskHuman 的完整交互协议恢复正常。
3. 不要求用户了解或关闭 Suggested prompts，也不新增 AskHuman 设置项。
4. 正常 `user` thread、用户创建的 `automation`、其它 MCP 客户端和旧版 Codex 保持现有行为。
5. 不伪造“用户已回答”或“用户已批准结束”的结果。

## 3. 非目标与边界

- 不修改 Codex / ChatGPT.app 本体；上游彻底隔离 `AGENTS.md` 和自定义 MCP 仍应单独反馈给 OpenAI。
- 不笼统屏蔽所有非 `user` thread；首期只匹配 Codex 元数据中的精确值 `system`。
- 不把 `ephemeral` 作为判据：当前 MCP request metadata 不包含该字段，且 `system` 已能精确覆盖本问题。
- 不用 ambient prompt 的完整原文作为工具层判据；提示词可能随桌面版更新。
- 不在工具层返回假的 human answer、假的结束批准或自动选择项。
- CLI 模式的 Shell 子进程只有 `CODEX_THREAD_ID`，没有 `thread_source`；ephemeral thread 又不保证写入
  可查询的 transcript/state DB。因此首期无法为 CLI 调用提供与 MCP 同等级的确定性硬拦截，CLI 依赖
  Rules 豁免。Codex 的 AskHuman 推荐集成仍是 MCP，确定性保障覆盖当前主要路径。

## 4. 方案总览

```text
Codex ambient thread (thread_source=system)
  │
  ├─ 加载更新后的 Codex 专属 AskHuman Rule
  │    └─ task-suggestion generator → 不调用 AskHuman
  │
  └─ 若模型仍调用 AskHuman MCP
       └─ AskHuman 从 tools/call._meta 读取 thread_source
            ├─ system → 本地拒绝；不 spawn CLI、不连 daemon、不写 todo
            └─ 其它/缺失 → 完全沿用现有处理
```

两层职责不同：Rules 让模型继续完成宿主要求的结构化建议，避免无意义重试；MCP guard 是不依赖 prompt
措辞的最终边界，保证当前 Codex MCP 路径不会真正联系用户。

## 5. Part A：Rules 增加非交互后台例外

### 5.1 Codex 专属最小文案

实现期重新核对完整 Suggested prompts 模板后，用户确认 Rule 不匹配 prompt 原文、数量、后台身份或
UI 行为，只把该 run 概括成 `task-suggestion generator`。Codex 的原 subagent 首句替换为一条最小规则：

```text
**This protocol does not apply to subagents or task-suggestion generators; if you are either, do not use AskHuman.**
```

第二条“启动 subagent 时告知身份”的规则不变。精确识别职责留给 MCP metadata guard；Rule 只负责让模型
理解该角色无需进入交互协议。

提示词管理改为“共享正文 + Agent 专属 scope rule”：`cli_reference_for(agent)` 与
`mcp_reference_for(agent)` 只在目标为 Codex 时生成上述首句。Claude、Cursor、Grok 继续生成原 subagent
首句，Grok skill 不携带 Codex 专属例外。通用手动参考提示词也保持原文不变。

### 5.2 更新传播

Rules 正文发生变化后，现有 `agent_rules::needs_update_variant()` 的精确比对会让旧安装显示需更新；
不新增 migration 状态或 UI。MCP guard 随 AskHuman 二进制更新即可生效，因此旧用户即使尚未更新 Rules，
也不会真的收到 MCP 弹窗；更新 Rules 后进一步避免后台 agent 重试或生成失败。

### 5.3 提示词测试

扩充 `prompts.rs` 与 `agent_rules.rs` 测试：

- Codex CLI / MCP 最终产物包含确认后的单句，并位于强制 “under all circumstances” 文案之前；
- Claude、Cursor、Grok CLI / MCP 产物及 Grok skill 均不包含该句；
- Rule 安装、更新和变体识别按目标 Agent 生成期望正文；
- 现有 subagent、提问、交付文件、todo、结束 marker 与 collaboration-style 断言全部保留。

## 6. Part B：MCP 根据可信 request metadata 硬拦截

### 6.1 元数据解析

在 `src-tauri/src/mcp/ask.rs` 增加局部纯函数，不新建跨模块抽象：

- 只读取 `_meta["x-codex-turn-metadata"]["thread_source"]`；
- 兼容 turn metadata 为 JSON object 或 JSON 字符串两种表示，便于跨 Codex 版本；
- 只对精确小写值 `system` 返回 true；
- 元数据缺失、格式错误、字段未知时 fail-open，沿用现有正常提问流程；
- 不信任或匹配顶层任意同名 `thread_source`，避免其它客户端偶然字段造成误拦截。

三个 tool handler 都接收 `RequestContext<RoleServer>`：

- `ask`：在 spawn AskHuman 子进程之前检查；
- `whats_next`：在 spawn 子进程之前检查；
- `todo_add`：在项目检测和落盘之前检查。

### 6.2 拦截结果

命中 `system` 时返回一个本地 MCP error result，正文固定说明：

```text
AskHuman is disabled for this Codex system-generated background thread.
Do not retry or contact the human; finish the host-requested non-interactive output directly.
```

使用 error result 而不是 fabricated answer，理由是：

- `ask` 声明的 output schema 只描述真实 human answer / cancel，不能塞入假的第三种回答；
- `whats_next` 的成功文本会被旧 Rules 当成“下一任务”或“结束批准”，语义不安全；
- 明确的 terminal error 保证不 spawn CLI、不连 daemon、不产生 popup/IM，即使模型忽略“不重试”，重试也
  只会再次本地拒绝。

Rules 的 Part A 是保障 ambient 生成质量的主路径；本 error 是防止联系用户的最后防线。

### 6.3 MCP 测试

增加两层测试：

1. 纯函数矩阵：object/string metadata、`system`、`user`、`automation`、字段缺失、畸形 JSON、伪造顶层字段。
2. 真实 rmcp 路由测试：构造带 `_meta` 的 `tools/call`，分别调用三个工具，断言：
   - 返回 `isError: true` 和固定 non-retry 文案；
   - `ask` / `whats_next` 没有 spawn 子进程；
   - `todo_add` 没有写 `todos.json`；
   - `user` 或无 metadata 时仍进入既有路径。

测试必须在临时 AskHuman home / cwd 下运行，不能弹真实窗口、不能连接生产 daemon。

## 7. 兼容性与风险

| 场景 | 行为 |
| --- | --- |
| 当前 Codex MCP + ambient `system` | Rules 避免调用；漏网调用被硬拦截 |
| 当前 Codex MCP + 普通 `user` | 完全不变 |
| Codex automation | `thread_source=automation`，不拦截 |
| 旧 Codex 不发送 turn metadata | 工具层 fail-open；Codex 专属 Rule 仍提供角色豁免 |
| Claude / Cursor / Grok | Rules 不含 Codex 专属句；MCP 没有 Codex 专用 meta key，完全不变 |
| Codex CLI 模式 | Rules 豁免生效；无确定性工具层 guard |
| 未来其它 Codex 内部 `system` run | AskHuman MCP 会静默拒绝；符合“宿主内部 run 不联系人类”的边界 |

主要风险是旧 Rules 与 hard error 同时存在时，后台模型可能重试并导致建议生成失败或超时；用户仍不会收到
提问。更新 Rules 后应消除该问题。若实际验证发现模型仍会调用，先验证 Codex 是否把该 run 识别为
task-suggestion generator，不改成 prompt 原文匹配或伪造用户批准。

## 8. 文档同步

实现时更新：

- `docs/specs/main-agent-only-interaction-protocol.md`：记录 Codex 专属 task-suggestion generator
  非交互例外与目标感知的 Rules 生成；
- `docs/specs/mcp.md`：补充 Codex per-call `_meta`、精确 `system` guard 和 fail-open 兼容策略；
- `docs/specs/todo-whats-next.md`：明确 ambient suggestion run 不是“完成一个用户任务”，不得触发
  `whats_next`；
- `docs/PROGRESS.md`：实现完成后删除本任务 section。

本改动不增加模块或改变仓库级运行架构，因此不修改 `docs/overview.md`。无需用户 wiki 或设置文案；功能应
自动生效而不是要求用户理解 Codex 的 Suggested prompts 开关。

## 9. 验证顺序

1. `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`。
2. 运行 `prompts` 与 `mcp::ask` 定向单测，包括 raw `tools/call._meta` 回归测试。
3. 运行 `cargo test --manifest-path src-tauri/Cargo.toml`。
4. 按仓库规定运行 `./scripts/install.sh`，把新二进制安装进当前环境。
5. 用新安装的 `AskHuman` 复核 Codex MCP 配置仍指向当前二进制、Rules 正文被正确标记为需更新，更新后
   `agents` / `doctor` 状态恢复正常。
6. 用无模型、无 popup 的 MCP 协议夹具发送 `thread_source=system` 调用，确认三个工具均本地静默拒绝。
7. 真正启用 Codex Suggested prompts 做端到端验证会启动计费模型并可能读取 connected apps；用户选择
   稍后自行验证，已加入项目 todo“验证 Codex Suggested prompts 后台线程不会触发 AskHuman”。

## 10. 完成条件

- 当前 Codex MCP ambient thread 无法触发 popup、IM 或 todo 写入；
- 普通用户 thread 的 AskHuman 行为和长时间阻塞语义无回归；
- 只有 Codex CLI/MCP 托管协议含 task-suggestion generator 例外，旧安装能沿现有机制提示更新；
- 元数据缺失或未知来源时保持兼容，不误伤其它客户端；
- 三份关联 spec 与 `docs/PROGRESS.md` 同步完成；
- `./scripts/install.sh` 成功，并使用新安装的 AskHuman 完成本地协议与 Rules 生成验收；真实桌面端验证
  由项目 todo 继续跟踪。
