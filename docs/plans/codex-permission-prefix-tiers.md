# Codex 权限前缀调档与宽松模式（实施计划）

状态：已完成（2026-07-24）。设计决策已并入
`docs/specs/codex-permission-remember.md`（D50-D52）并在 `docs/PROGRESS.md` 登记；
本文档留档备查。

## 背景与问题

Codex 权限 Hook 的「始终允许」前缀经常带满具体参数（原生派生逻辑直接取第一条需审批命令
原样，从不泛化），导致同类命令反复弹窗。上游 0.145.0 两个关键变化：

- #34271：`BANNED_PREFIX_SUGGESTIONS` 大幅扩容（新禁 `rm`、`git`、`sudo`、`env`、
  `npm run` 等），并一次性迁移删除了历史违禁 exact 规则。
- #32232：权限 Hook 在 guardian（strict auto-review）之前运行，Hook 的 allow 即最终
  裁决，官方有集成测试背书。

## 已确认决策（AskHuman，2026-07-24）

| # | 议题 | 决定 |
|---|------|------|
| Q1 | D43 保守处理 | 跟随上游放宽：`request_permissions` 出现不再禁用记忆选项 |
| Q2 | 前缀调档交互 | 弹窗内共享档位选择器；IM 卡片不加选择器，直接用推荐档 |
| Q3 | 自动推荐档位 | 智能 2-token（命令+子命令）；第二词是 flag/路径/数字时退 1-token；所有候选一律预校验 |
| Q4 | 宽松模式开启方式 | 弹窗内「本会话开启」+ 设置页全局开关，两者都要 |
| Q5 | 宽松模式作用域 | V1 仅 shell 请求 |
| Q6 | 危险清单基线 | 认可基线：任意 `rm`、`dd`、`mkfs*`、`git reset --hard`、`git clean`、`git push --force` 等数据丢失类（可配置预留） |

## 方案与进度

### A. 同步复刻（已完成 ✅）

- `shell_safety.rs`：`BANNED_PREFIX_SUGGESTIONS` 同步 0.145.0 扩容表；
  `VERIFIED_CODEX_VERSION_CEILING` → (0, 146)，audit 基线 upstream main `1a817bb95d`。
- `permission_memory.rs`：删除 `request_permissions_risk` 字段与全部关联逻辑
  （D43 → 按 D50 放宽）；`evaluate_gate` reviewer=user 分支无条件 `MemoryAllowed`；
  相关测试已改写并通过。

### C. Worker 候选派生（已完成 ✅）

`permission_shell.rs` `ShellWorkerOutput` 新增字段（serde default，向后兼容）：

- `amendment_candidates: Vec<Vec<String>>`：base amendment 在前，随后按长度递减列出
  通过校验的截断。校验 = 不在禁选表 + 自身非危险命令 + 覆盖全部段（段被前缀命中，或
  该段独立评估为 allow）。`amendment` 为 None 时恒为空。
- `segment_allows: Vec<bool>`：与 `segments` 平行；该段所有 policy 决策 + 启发式回退
  全为 allow。
- `policy_prompt_any: bool`：任一段命中显式 prompt 规则（原生必弹；宽松模式必须禁用，
  见 D 部分）。

测试：`amendment_candidates_list_validated_truncations`、
`amendment_candidates_skip_dangerous_truncations` 等 15 个全部通过。

### B. 协议扩展 + 弹窗调档（已完成 ✅）

**协议（models.rs，已完成）**：`ConfirmChoice` 新增
`variant: Option<ChoiceVariant>`，`ChoiceVariant { group, level, level_label, segment_label,
recommended }`。`segment_label` 是相对上一更短候选新增的完整 token 块；若中间长度被
安全过滤，跨过的 token 合并为一个不可拆分 segment。`level_label` 保留整档位兼容摘要标签。
同一 `group` 的 choice 是同一逻辑动作的不同档位；弹窗折叠成一行 + 共享选择器；
其余渠道只渲染 `recommended` 档。单候选时 `variant: None`，一切保持现状。

**档位平铺（permission_memory.rs `shell_enhancement`，已完成）**：

- 阶梯 = `amendment_candidates` 反转（最短在前）；`level` 为阶梯下标，跨 group 共享。
- 推荐档 `recommended_candidate_index`（Q3）：base 第二词 `subcommand_like`
  （不以 `-` 开头、不含 `/`、非数字）→ 期望长度 2，否则 1；候选缺失该长度时向更长
  的候选升档（绝不降档），兜底 base。
- action id：推荐档保持原 id（`remember_shell_prefix` / `remember_shell_always`），
  其余 `remember_shell_prefix:<candidates 下标>`。IM 回调、历史、daemon 校验零改动
  （`memory.saves` 按 action_id 一一对应，每档独立 `MemorySave`）。
- session 档 → `RuleKey::ShellPrefix`；permanent 档 → `NativeWrite::PrefixRule`。
- 危险命令（`dangerous_any`）仍禁 session 档与 auto-allow query（D38 不变）。

**IM 卡片可见性（choice_cards.rs / channels/confirm.rs，已完成）**：

- `task_choice_indices` 与 `telegram_keyboard` 统一经 `variant_visible` 过滤非推荐档；
  任务表单键盘保留全部可操作按钮（列表隐藏、按钮不隐藏的原语义不变）。
- Telegram 编号 marker 改按可见位置计数（列表与按钮一致，无档位时行为不变）；
  回调仍携带 wire index。
- Slack 长文案 detail 区、`telegram_uses_full_labels` 同步只看可见项。
- 钉钉：卡片 option id 是「下发数组内位置」，非任务分支 options 改为可见列表，
  提交端按 `task_choice_indices` 做位置→wire 翻译；`deny_index` 同步译为可见位置
  （任务卡不含 dismiss，保留 wire 值兜底，与旧行为一致）。
- 顺带修复：IM 终态文案「已拒绝」原按 `role == Destructive` 判定，现改按
  `action_id == dismiss_action_id`（完全磁盘 / 宽松模式等广授权选项复用
  Destructive 仅作危险配色，不再误报为拒绝）。

**弹窗（已完成）**：`types.ts` 加 `ChoiceVariant`；`usePopupCore.ts` 新增
`confirmRows`（同 group 折叠为一行）、`confirmVariantLevels`（选择器档位，标签取
`segmentLabel`，旧 daemon 回退 `levelLabel`）、`confirmVariantLevel`（共享档位，加载时
停在推荐档）、`selectConfirmVariantLevel`（切档时已选行跟随换 wire index）；⌘1-9 快捷键
按展示行计数。`ConfirmPane.vue` 渲染无间隙 token 轨道：当前边界前连续高亮，蓝点标识已提交
档位，hover 只预览高亮，点击或左右键选档，`Reset` 回推荐档；长 token 单段省略、整条轨道
横向滚动。样式在 `popup.css`（`.confirm-variant-*`）。历史视图不渲染 choices，无需处理。

**测试（已完成）**：`multi_candidate_ladder_flattens_variants_with_recommended_default`、
`recommended_candidate_prefers_two_tokens_then_escalates`、
`cards_show_only_recommended_prefix_variants_with_wire_callbacks` 等。

### D. 宽松模式「只审危险」（已完成 ✅）

- `shell_safety.rs`：`is_relaxed_dangerous_command`（独立于上游复刻，仅供宽松模式，
  包装深度超限 fail-closed 记危险）：任意 `rm`/`srm`/`shred`/`dd`/`diskutil`/
  `truncate`/`mkfs*`、`find -delete`、`xargs`（逐后缀递归查危险目标）、
  `git reset --hard`/`clean`/`checkout --`/`checkout .`/`restore`/
  `push --force(-with-lease)/-f`/`branch -D`/`stash drop|clear`、
  `kill`/`pkill`/`killall`/`shutdown`/`reboot`/`halt`/`poweroff`、
  `launchctl bootout|unload|remove`、`chmod -R`/`chown -R`；
  sudo/env/trap 包装与 `bash -c` 字面脚本递归检查。
- `permission_rules.rs`：`RuleKey::ShellRelaxed`（会话级，30 天滚动 TTL 同其它规则）；
  `MemoryQuery::ShellCommands` 加 `relaxed_eligible: bool`（serde default）；
  `check_relaxed_auto_allow(session_id)`（命中刷新 TTL；不参与 `query_hits`）。
- eligibility 由 hook 端计算（`shell_enhancement`）：`!policy_prompt_any &&
  !dangerous_any && 无段命中扩展危险清单`（可拆分由 segments 非空隐含；
  rollout gate MemoryAllowed 由 Enhanced 路径隐含）。
- 弹窗选项「本对话开启宽松模式：只审危险命令」（`remember_shell_relaxed`，
  Destructive 危险配色）：仅 eligible 弹窗出现；保存后同类请求 daemon 侧直接
  auto-allow，弹窗不再出现该选项（无需去重逻辑）。
- 全局开关：`AppConfig.permissions.codex_relaxed_shell`（默认关）；设置页
  「高级 → Codex 会话授权」卡顶部 toggle（`PermissionRulesCard.vue`）。
- daemon `handle_submit_confirm`：规则 auto-allow miss 后，若
  `relaxed_eligible && (全局开 || 会话规则)` → 以 `AUTO_ALLOW_ACTION_ID` 放行并写
  审计日志（`relaxed auto-allow (global|session) for session …: <完整命令>`）。
- 管理面板：kind `shellRelaxed`（i18n「宽松模式（只审危险命令）」），计入 shell 规则数。

### 收尾（已完成 ✅）

- [x] `docs/specs/codex-permission-remember.md` 新增决策条目：D50（D43 放宽）、
  D51（前缀候选阶梯 + 智能推荐 + variant 协议）、D52（宽松模式）。
- [x] `docs/PROGRESS.md` 登记。
- [x] `cargo test --bin AskHuman` 全量（970 通过）+ clippy 干净 + 前端
  `npm run build` / `npm test`（81 通过）。
- [x] 实测（2026-07-24，release 构建换新 daemon 后端到端验证）：
  1. `cargo build --release` 弹窗档位选择器，推荐档 `cargo build` 落库 session 前缀；
  2. 同会话 `cargo build --package foo --verbose` 记忆规则静默放行；
  3. `make test` 弹窗宽松模式选项，开启后 `shellRelaxed` 落库；
  4. `python3 setup.py check && npm run lint` 宽松模式自动放行，审计日志含完整命令；
  5. 宽松模式下 `rm -rf build` 仍弹窗（扩展危险清单命中），未被静默放行。

## 后续追加：YOLO 模式（D53，2026-07-25，已完成 ✅）

同日用户追加需求：会话级「自动允许一切」。设计定案（AskHuman Q&A）：范围＝该会话全部
Codex 权限请求（含危险命令 / 文件 / MCP / 网络）；入口＝所有 Codex 权限弹窗末尾选项；
不做全局开关（等效原生 `--yolo`）；关闭＝管理面板专属按钮（`DisableYolo`，只删 Yolo 规
则）+ IM `/yolo` 命令（无参 → 带关闭按钮的会话选择卡 `PickerKind::Yolo`，
`off [编号]` 直关）+ IM 开启卡终态附关闭提示。实现细节固化在 spec D53；实现横跨
`permission_rules.rs`（`RuleKey::Yolo`/`check_yolo_auto_allow`/`disable_yolo`/
`yolo_session_ids` + 摘要 `yolo` 标志）、`permissions.rs`（choice 注入 + Basic 弹窗补
`memory`、`AUTO_ALLOW` 接受条件放宽为 memory 存在）、daemon `handle_submit_confirm`
（规则/宽松 miss 后查 Yolo，审计日志）、`autochannel.rs`（`Command::Yolo` 解析 + help）、
`inbound.rs`（`handle_yolo_cmd`）、`select.rs`×2（`SelectAction::Yolo` 渲染 + 三路分发
`select_pick_yolo`）、`channels/confirm.rs`（终态提示）、设置面板前后端（徽标 + 按钮 +
`disableYolo` op）。测试：规则层生命周期、hook 注入/决策映射、`/yolo` 解析、help 列出，
全量 cargo test 975 通过 + 前端 81 通过。

## 关键实现注意

- `ShellWorkerOutput` 新字段全部 `#[serde(default)]`：worker/hook 版本错配时优雅退化。
- 候选覆盖校验里「段独立 allow」用 `segment_allows`（policy allow 或启发式 allow
  均可），比原生 prefix_rule 派生（仅启发式 allow）略宽——这是我们自己的扩展面，
  候选仍全部经过禁选/危险校验。
- 弹窗提交走 `choice_index`（wire index），`memory_finalizer` 按 action_id 找
  `MemorySave` 做两阶段提交——所以平铺方案对 daemon 完全透明。
- IM 卡片 `task_choice_indices` 是所有渠道渲染的唯一可见性入口，过滤加在这里即可；
  回调数据里嵌的是真实 wire index，不受过滤影响。
