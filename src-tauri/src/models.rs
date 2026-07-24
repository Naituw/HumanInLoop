//! 核心数据模型（与前端 TS 类型一一对应，serde 采用 camelCase）。

use serde::{Deserialize, Serialize};

/// 标题来源名的默认值（环境变量未设置或为空时使用）。
pub const DEFAULT_SOURCE_NAME: &str = "the Loop";
/// Custom caller name used by Popup and ordinary IM Message / Question titles.
pub const SOURCE_NAME_ENV: &str = "ASKHUMAN_ENV_SOURCE_NAME";

/// 读取来源名：「Question from {source_name}」。环境变量为空或缺省时回退默认值。
pub fn source_name() -> String {
    std::env::var(SOURCE_NAME_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SOURCE_NAME.to_string())
}

/// 解析来源名（考虑探测到的调用方 Agent）。
///
/// 优先级：自定义环境变量来源名 > 探测到的 Agent 展示名（Claude Code / Codex / Cursor）
/// > 默认 "the Loop"。供弹窗标题与各渠道消息头共用，使「未定制来源名时显示发起 Agent」。
pub fn source_name_for_agent(agent: Option<crate::agents::AgentKind>) -> String {
    let custom = std::env::var(SOURCE_NAME_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    resolve_source(custom.as_deref(), agent)
}

/// 来源名解析的纯逻辑（不读 env，便于测试）。`custom` 为已 trim 的非空自定义来源名。
fn resolve_source(custom: Option<&str>, agent: Option<crate::agents::AgentKind>) -> String {
    if let Some(name) = custom.filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    if let Some(kind) = agent {
        return kind.label().to_string();
    }
    DEFAULT_SOURCE_NAME.to_string()
}

/// 结果输出格式（全局，对所有问题生效）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// 现有文本区块（字段英文、不本地化）。
    #[default]
    Text,
    /// 结构化 JSON（snake_case、省空字段、美化多行）。
    Json,
}

/// 一次提问请求：一个共享 Message（描述 + 附件）+ 一组问题（恒 ≥1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskRequest {
    pub id: String,
    /// 是否按 Markdown 渲染（全局，对所有问题生效）。
    pub is_markdown: bool,
    /// 共享 Message：所有问题的描述与展示附件。
    pub message: MessagePrompt,
    /// 问题列表（恒 ≥1，由 CLI 归一化保证：无 `-q` 时由第一个参数提升而来）。
    #[serde(default)]
    pub questions: Vec<Question>,
    /// 严格选择：禁用自由文本 / 回复附件，只能勾选预设项（全局）。
    #[serde(default)]
    pub select_only: bool,
    /// 单选：每题恰好一个选择（默认多选，全局）。
    #[serde(default)]
    pub single: bool,
    /// 结果输出格式（全局）。
    #[serde(default)]
    pub output_format: OutputFormat,
    /// whats-next 提问（spec todo-whats-next D2/D3）：结果渲染为一段纯文本（任务内容 /
    /// 固定结束句），弹窗折叠待办区不重复渲染 chip。普通提问恒 false（序列化省略）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub whats_next: bool,
}

impl AskRequest {
    pub fn new(message: MessagePrompt, questions: Vec<Question>, is_markdown: bool) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            is_markdown,
            message,
            questions,
            select_only: false,
            single: false,
            output_format: OutputFormat::Text,
            whats_next: false,
        }
    }
}

/// 共享 Message：所有问题的描述文本与展示附件（不持有选项）。
///
/// `text` 仅在使用了 `-q`（存在独立描述）时非空；无 `-q` 时第一个参数被提升为问题，`text` 为空。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePrompt {
    #[serde(default)]
    pub text: String,
    /// 提问附带的文件（AI→人，仅用于弹窗展示，不进入结果输出）。
    #[serde(default)]
    pub files: Vec<FileAttachment>,
}

impl MessagePrompt {
    pub fn new(text: String, files: Vec<FileAttachment>) -> Self {
        Self { text, files }
    }
}

/// 单个问题（其选项与该问题绑定；是否 Markdown 见 `AskRequest::is_markdown`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub message: String,
    #[serde(default)]
    pub predefined_options: Vec<OptionItem>,
}

impl Question {
    pub fn new(message: String, predefined_options: Vec<OptionItem>) -> Self {
        Self {
            message,
            predefined_options,
        }
    }
}

/// 单个预定义选项：文本 + 是否为提问方（AI）的推荐答案。
///
/// 序列化恒为对象形态；反序列化兼容旧格式的纯字符串
/// （旧 history.jsonl / 旧 IPC 负载 → `recommended=false`，零迁移）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionItem {
    pub text: String,
    pub recommended: bool,
    /// 该选项承载的待办条目 id（spec todo-whats-next D2/D5）：whats-next / Stop 卡把项目待办
    /// 渲染为选项时携带；赢家回答选中带此 id 的选项 → Coordinator 在终态汇聚点原子出队。
    /// 普通选项恒 None（序列化省略，旧端零感知）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_id: Option<String>,
}

impl OptionItem {
    pub fn new(text: impl Into<String>, recommended: bool) -> Self {
        Self {
            text: text.into(),
            recommended,
            todo_id: None,
        }
    }

    /// 携带待办条目 id 的选项（whats-next / Stop 卡的待办 chip）。
    pub fn with_todo(text: impl Into<String>, todo_id: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            recommended: false,
            todo_id: Some(todo_id.into()),
        }
    }
}

impl<'de> Deserialize<'de> for OptionItem {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // 兼容两种输入：纯字符串（旧格式）与对象（recommended 缺省 false）。
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Object {
                text: String,
                #[serde(default)]
                recommended: bool,
                #[serde(default, rename = "todoId")]
                todo_id: Option<String>,
            },
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Text(text) => OptionItem {
                text,
                recommended: false,
                todo_id: None,
            },
            Raw::Object {
                text,
                recommended,
                todo_id,
            } => OptionItem {
                text,
                recommended,
                todo_id,
            },
        })
    }
}

/// 提问附带的文件附件（展示用）。`path` 为绝对路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachment {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_image: bool,
}

/// 图片附件。`data` 为 base64，可带 `data:...;base64,` 前缀（落盘时清理）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachment {
    pub data: String,
    pub media_type: String,
    #[serde(default)]
    pub filename: Option<String>,
}

/// Channel 的终态动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChannelAction {
    Send,
    Cancel,
}

/// 对单个问题的回答。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionAnswer {
    #[serde(default)]
    pub selected_options: Vec<String>,
    #[serde(default)]
    pub user_input: Option<String>,
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
    /// 用户随回复附带的本地文件绝对路径（非图片，直接透传不复制）。
    #[serde(default)]
    pub files: Vec<String>,
    /// Popup 折叠待办区选中的待办条目 id（spec todo-whats-next D7）：其文本已由前端并入
    /// `user_input` 送达；此字段只供 Coordinator 在终态汇聚点按 id 出队。恒为空时序列化省略。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todo_ids: Vec<String>,
}

impl QuestionAnswer {
    /// 是否为空回答：没选项、没（去空白后的）输入、没图片、没回复文件。
    pub fn is_empty(&self) -> bool {
        self.selected_options.is_empty()
            && self
                .user_input
                .as_deref()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
            && self.images.is_empty()
            && self.files.is_empty()
    }
}

/// 某个 Channel 给出的最终回答（按问题顺序，每题一项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelResult {
    pub action: ChannelAction,
    #[serde(default)]
    pub answers: Vec<QuestionAnswer>,
    pub source_channel_id: String,
}

// MARK: - Structured confirmations

/// Stable semantic kind for a confirmation context field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConfirmFieldKind {
    Text,
    Path,
    Timestamp,
}

/// A required, independently rendered piece of confirmation context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmField {
    pub id: String,
    pub label: String,
    pub value: String,
    pub kind: ConfirmFieldKind,
}

/// Human-readable confirmation detail. `summary` is always preserved while `body_md` may be
/// budgeted by individual channel renderers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmDetail {
    pub summary: String,
    #[serde(default)]
    pub body_md: String,
}

/// One stable action exposed by a structured confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmChoice {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub role: crate::confirm::ActionRole,
    /// Prefix-tier variant metadata (D51). Choices sharing a `group` are one logical
    /// action at different generalization levels: the popup collapses them into a
    /// single row with a shared level selector, every other surface renders only the
    /// `recommended` variant. `None` for plain choices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<ChoiceVariant>,
}

/// See [`ConfirmChoice::variant`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceVariant {
    /// Logical action key shared by all levels (e.g. "shell-prefix-session").
    pub group: String,
    /// Ladder position; the same level index selects together across groups.
    pub level: usize,
    /// Short selector label for this level (the truncated prefix text).
    pub level_label: String,
    /// The auto-recommended level (smart 2-token, D51): the only variant that
    /// non-popup surfaces show.
    pub recommended: bool,
}

/// Optional input attached to a structured choice form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmInput {
    pub id: String,
    pub visible_when_action_id: String,
    /// Keep the input visible for every action while still using
    /// `visible_when_action_id` as the action that requires text.
    #[serde(default)]
    pub always_visible: bool,
    /// Require non-empty input when `visible_when_action_id` is selected.
    #[serde(default)]
    pub required: bool,
    /// Fixed task-prefix length for actions that append this input after existing text.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub prefix_chars_by_action_id: std::collections::BTreeMap<String, usize>,
    pub label: String,
    #[serde(default)]
    pub placeholder: String,
    pub max_chars: usize,
}

impl ConfirmInput {
    /// Preserve the original long task-input contract for older serialized requests while
    /// allowing an explicitly always-visible input to be optional for non-manual actions.
    pub fn requires_value(&self) -> bool {
        self.required || (!self.always_visible && self.max_chars > 1000)
    }

    pub fn max_value_chars_for(&self, action_id: &str) -> usize {
        let prefix = self
            .prefix_chars_by_action_id
            .get(action_id)
            .copied()
            .unwrap_or(0);
        if prefix == 0 {
            self.max_chars
        } else {
            self.max_chars.saturating_sub(prefix.saturating_add(2))
        }
    }
}

/// Presentation contract for the first structured confirmation surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ConfirmPresentation {
    SingleSelectSubmit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<ConfirmInput>,
        submit_label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_action_id: Option<String>,
    },
}

impl ConfirmPresentation {
    pub fn input(&self) -> Option<&ConfirmInput> {
        match self {
            Self::SingleSelectSubmit { input, .. } => input.as_ref(),
        }
    }

    pub fn submit_label(&self) -> &str {
        match self {
            Self::SingleSelectSubmit { submit_label, .. } => submit_label,
        }
    }

    pub fn default_action_id(&self) -> Option<&str> {
        match self {
            Self::SingleSelectSubmit {
                default_action_id, ..
            } => default_action_id.as_deref(),
        }
    }
}

/// Caller-supplied semantic confirmation. It deliberately contains no request id or deadline;
/// those are assigned by the daemon after validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmSpec {
    pub title: String,
    #[serde(default)]
    pub context: Vec<ConfirmField>,
    pub detail: ConfirmDetail,
    pub choices: Vec<ConfirmChoice>,
    pub presentation: ConfirmPresentation,
    pub dismiss_action_id: String,
}

/// Daemon-owned structured confirmation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmRequest {
    pub id: String,
    pub title: String,
    pub context: Vec<ConfirmField>,
    pub detail: ConfirmDetail,
    pub choices: Vec<ConfirmChoice>,
    pub presentation: ConfirmPresentation,
    pub dismiss_action_id: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

/// Shared envelope for surfaces that can render either a question or a structured confirmation.
/// The two business protocols remain independent; this enum only unifies delivery and display.
// Boxing the large Ask variant would ripple through every construct/match site; envelopes are
// short-lived and few, so the size gap is acceptable.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "request", rename_all = "camelCase")]
pub enum InteractionRequest {
    Ask(AskRequest),
    Confirm(ConfirmRequest),
}

impl InteractionRequest {
    pub fn id(&self) -> &str {
        match self {
            Self::Ask(request) => &request.id,
            Self::Confirm(request) => &request.id,
        }
    }

    pub fn ask(&self) -> Option<&AskRequest> {
        match self {
            Self::Ask(request) => Some(request),
            Self::Confirm(_) => None,
        }
    }

    pub fn confirm(&self) -> Option<&ConfirmRequest> {
        match self {
            Self::Ask(_) => None,
            Self::Confirm(request) => Some(request),
        }
    }
}

/// A validated terminal choice. `action_id` is resolved from the daemon-owned choice ledger, not
/// accepted from a channel callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmResult {
    pub action_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub source_channel_id: String,
}

/// Stable reasons for returning no human decision to a confirmation caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConfirmFallbackReason {
    NoAvailableChannel,
    Expired,
    InvalidRequest,
    Draining,
    InternalError,
}

/// Channel delivery state for a structured confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmDeliveryState {
    Starting,
    Ready { message_id: String },
    Failed { reason: String },
    Terminal,
}

/// Shared visual choice form used by Ask and Confirm adapters. It contains presentation data only;
/// business results still use the independent Ask/Confirm protocols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceFormView {
    pub options: Vec<ChoiceFormOption>,
    pub single: bool,
    pub submit_label: String,
    pub default_index: Option<usize>,
    pub input: Option<ChoiceFormInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceFormOption {
    pub wire_index: usize,
    pub label: String,
    pub description: String,
    pub role: crate::confirm::ActionRole,
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceFormInput {
    pub id: String,
    pub visibility: ChoiceFormInputVisibility,
    pub label: String,
    pub placeholder: String,
    pub max_chars: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceFormInputVisibility {
    Always,
    WhenIndex(usize),
}

impl ChoiceFormView {
    pub fn from_ask(request: &AskRequest, submit_label: impl Into<String>) -> Vec<Self> {
        let submit_label = submit_label.into();
        request
            .questions
            .iter()
            .enumerate()
            .map(|(question_index, question)| Self {
                options: question
                    .predefined_options
                    .iter()
                    .enumerate()
                    .map(|(wire_index, option)| ChoiceFormOption {
                        wire_index,
                        label: option.text.clone(),
                        description: String::new(),
                        role: crate::confirm::ActionRole::Default,
                        recommended: option.recommended,
                    })
                    .collect(),
                single: request.single,
                submit_label: submit_label.clone(),
                default_index: None,
                input: (!request.select_only).then(|| ChoiceFormInput {
                    id: format!("ask_input_{question_index}"),
                    visibility: ChoiceFormInputVisibility::Always,
                    label: String::new(),
                    placeholder: String::new(),
                    max_chars: usize::MAX,
                }),
            })
            .collect()
    }
}

impl ConfirmSpec {
    /// Validate the stable contract before a daemon request id or deadline is allocated.
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("confirm title must not be empty".to_string());
        }
        if self.detail.summary.trim().is_empty() {
            return Err("confirm summary must not be empty".to_string());
        }
        if self.choices.len() < 2 {
            return Err("confirm requires at least two choices".to_string());
        }

        let mut choice_ids = std::collections::HashSet::new();
        for choice in &self.choices {
            if choice.id.trim().is_empty() || choice.label.trim().is_empty() {
                return Err("confirm choice id and label must not be empty".to_string());
            }
            if !choice_ids.insert(choice.id.as_str()) {
                return Err(format!("duplicate confirm choice id: {}", choice.id));
            }
        }
        if !choice_ids.contains(self.dismiss_action_id.as_str()) {
            return Err("dismiss action must reference a confirm choice".to_string());
        }
        if let Some(default_id) = self.presentation.default_action_id() {
            if !choice_ids.contains(default_id) {
                return Err("default action must reference a confirm choice".to_string());
            }
        }
        if let Some(input) = self.presentation.input() {
            if input.id.trim().is_empty() || input.max_chars == 0 {
                return Err("confirm input requires an id and positive max_chars".to_string());
            }
            if !choice_ids.contains(input.visible_when_action_id.as_str()) {
                return Err("confirm input action must reference a confirm choice".to_string());
            }
        }

        let mut field_ids = std::collections::HashSet::new();
        for field in &self.context {
            if field.id.trim().is_empty()
                || field.label.trim().is_empty()
                || field.value.trim().is_empty()
            {
                return Err("confirm context fields must be complete".to_string());
            }
            if !field_ids.insert(field.id.as_str()) {
                return Err(format!("duplicate confirm context id: {}", field.id));
            }
        }
        Ok(())
    }

    pub fn into_request(
        self,
        id: String,
        created_at_ms: u64,
        expires_at_ms: u64,
    ) -> Result<ConfirmRequest, String> {
        self.validate()?;
        if id.trim().is_empty() {
            return Err("confirm request id must not be empty".to_string());
        }
        if expires_at_ms <= created_at_ms {
            return Err("confirm expiry must be after creation".to_string());
        }
        Ok(ConfirmRequest {
            id,
            title: self.title,
            context: self.context,
            detail: self.detail,
            choices: self.choices,
            presentation: self.presentation,
            dismiss_action_id: self.dismiss_action_id,
            created_at_ms,
            expires_at_ms,
        })
    }
}

impl ConfirmRequest {
    pub fn choice_form_view(&self) -> ChoiceFormView {
        let default_index = self
            .presentation
            .default_action_id()
            .and_then(|id| self.choices.iter().position(|c| c.id == id));
        let input = self.presentation.input().and_then(|input| {
            self.choices
                .iter()
                .position(|c| c.id == input.visible_when_action_id)
                .map(|visible_when_index| ChoiceFormInput {
                    id: input.id.clone(),
                    visibility: if input.always_visible {
                        ChoiceFormInputVisibility::Always
                    } else {
                        ChoiceFormInputVisibility::WhenIndex(visible_when_index)
                    },
                    label: input.label.clone(),
                    placeholder: input.placeholder.clone(),
                    max_chars: input.max_chars,
                })
        });
        ChoiceFormView {
            options: self
                .choices
                .iter()
                .enumerate()
                .map(|(wire_index, choice)| ChoiceFormOption {
                    wire_index,
                    label: choice.label.clone(),
                    description: choice.description.clone(),
                    role: choice.role,
                    recommended: false,
                })
                .collect(),
            single: true,
            submit_label: self.presentation.submit_label().to_string(),
            default_index,
            input,
        }
    }

    /// Resolve a channel wire index into a stable action id and enforce conditional input rules.
    pub fn resolve_submission(
        &self,
        choice_index: usize,
        comment: Option<String>,
        source_channel_id: impl Into<String>,
    ) -> Result<ConfirmResult, String> {
        let choice = self
            .choices
            .get(choice_index)
            .ok_or_else(|| "confirm choice index out of range".to_string())?;
        let comment = match self.presentation.input() {
            Some(input) if input.always_visible || input.visible_when_action_id == choice.id => {
                let value = comment.unwrap_or_default().trim().to_string();
                let prefix_chars = input
                    .prefix_chars_by_action_id
                    .get(&choice.id)
                    .copied()
                    .unwrap_or(0);
                let combined_chars = prefix_chars.saturating_add(if value.is_empty() {
                    0
                } else {
                    2usize.saturating_add(value.chars().count())
                });
                if combined_chars > input.max_chars {
                    return Err(format!(
                        "confirm input exceeds {} characters",
                        input.max_chars
                    ));
                }
                if input.requires_value()
                    && input.visible_when_action_id == choice.id
                    && value.is_empty()
                {
                    return Err("confirm input is required".to_string());
                }
                (!value.is_empty()).then_some(value)
            }
            _ => None,
        };
        let source_channel_id = source_channel_id.into();
        if source_channel_id.trim().is_empty() {
            return Err("confirm result requires a source channel".to_string());
        }
        Ok(ConfirmResult {
            action_id: choice.id.clone(),
            comment,
            source_channel_id,
        })
    }

    pub fn dismiss_index(&self) -> usize {
        self.choices
            .iter()
            .position(|choice| choice.id == self.dismiss_action_id)
            .expect("validated confirm request must contain dismiss action")
    }

    pub fn input_max_chars_for_choice(&self, choice_index: usize) -> Option<usize> {
        let choice = self.choices.get(choice_index)?;
        self.presentation
            .input()
            .map(|input| input.max_value_chars_for(&choice.id))
    }
}

impl ChannelResult {
    pub fn cancel(source_channel_id: impl Into<String>) -> Self {
        Self {
            action: ChannelAction::Cancel,
            answers: Vec::new(),
            source_channel_id: source_channel_id.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;

    #[test]
    fn resolve_source_prefers_custom_over_agent() {
        assert_eq!(
            resolve_source(Some("MyAgent"), Some(AgentKind::Cursor)),
            "MyAgent"
        );
    }

    #[test]
    fn resolve_source_falls_back_to_agent_label() {
        assert_eq!(resolve_source(None, Some(AgentKind::Cursor)), "Cursor");
        assert_eq!(resolve_source(None, Some(AgentKind::Codex)), "Codex");
        assert_eq!(resolve_source(None, Some(AgentKind::Claude)), "Claude Code");
    }

    #[test]
    fn resolve_source_defaults_to_the_loop() {
        assert_eq!(resolve_source(None, None), DEFAULT_SOURCE_NAME);
        // 空自定义名视同未设置，回退 Agent / 默认。
        assert_eq!(resolve_source(Some(""), Some(AgentKind::Cursor)), "Cursor");
    }

    #[test]
    fn option_item_deserializes_legacy_string() {
        let o: OptionItem = serde_json::from_str("\"继续\"").unwrap();
        assert_eq!(o, OptionItem::new("继续", false));
    }

    #[test]
    fn option_item_deserializes_object_with_default_recommended() {
        let o: OptionItem = serde_json::from_str(r#"{"text":"A"}"#).unwrap();
        assert_eq!(o, OptionItem::new("A", false));
        let o: OptionItem = serde_json::from_str(r#"{"text":"A","recommended":true}"#).unwrap();
        assert_eq!(o, OptionItem::new("A", true));
    }

    #[test]
    fn option_item_serializes_as_object() {
        let s = serde_json::to_string(&OptionItem::new("A", true)).unwrap();
        assert_eq!(s, r#"{"text":"A","recommended":true}"#);
    }

    #[test]
    fn question_deserializes_mixed_legacy_and_object_options() {
        // 旧字符串数组、对象数组与混合数组均可读出。
        let q: Question = serde_json::from_str(
            r#"{"message":"Q","predefinedOptions":["A",{"text":"B","recommended":true}]}"#,
        )
        .unwrap();
        assert_eq!(
            q.predefined_options,
            vec![OptionItem::new("A", false), OptionItem::new("B", true)]
        );
    }

    fn confirm_spec() -> ConfirmSpec {
        ConfirmSpec {
            title: "Permission request".into(),
            context: vec![ConfirmField {
                id: "agent".into(),
                label: "Agent".into(),
                value: "Claude Code".into(),
                kind: ConfirmFieldKind::Text,
            }],
            detail: ConfirmDetail {
                summary: "Run a command".into(),
                body_md: "```sh\ngit status\n```".into(),
            },
            choices: vec![
                ConfirmChoice {
                    id: "approve_once".into(),
                    label: "Approve once".into(),
                    description: String::new(),
                    role: crate::confirm::ActionRole::Primary,
                    variant: None,
                },
                ConfirmChoice {
                    id: "deny".into(),
                    label: "Deny".into(),
                    description: String::new(),
                    role: crate::confirm::ActionRole::Destructive,
                    variant: None,
                },
            ],
            presentation: ConfirmPresentation::SingleSelectSubmit {
                input: Some(ConfirmInput {
                    id: "reason".into(),
                    visible_when_action_id: "deny".into(),
                    always_visible: false,
                    required: false,
                    prefix_chars_by_action_id: Default::default(),
                    label: "Reason".into(),
                    placeholder: String::new(),
                    max_chars: 1000,
                }),
                submit_label: "Submit".into(),
                default_action_id: None,
            },
            dismiss_action_id: "deny".into(),
        }
    }

    #[test]
    fn confirm_spec_validates_stable_ids() {
        assert!(confirm_spec().validate().is_ok());

        let mut duplicate = confirm_spec();
        duplicate.choices[1].id = "approve_once".into();
        assert!(duplicate.validate().unwrap_err().contains("duplicate"));

        let mut bad_input = confirm_spec();
        match &mut bad_input.presentation {
            ConfirmPresentation::SingleSelectSubmit { input, .. } => {
                input.as_mut().unwrap().visible_when_action_id = "missing".into();
            }
        }
        assert!(bad_input.validate().unwrap_err().contains("input action"));
    }

    #[test]
    fn daemon_owned_confirm_fields_are_added_after_validation() {
        let request = confirm_spec()
            .into_request("req-1".into(), 1_000, 2_000)
            .unwrap();
        assert_eq!(request.id, "req-1");
        assert_eq!(request.created_at_ms, 1_000);
        assert_eq!(request.expires_at_ms, 2_000);
        assert_eq!(request.dismiss_index(), 1);
        assert_eq!(request.choice_form_view().default_index, None);
        assert_eq!(
            request.choice_form_view().input.unwrap().visibility,
            ChoiceFormInputVisibility::WhenIndex(1)
        );
    }

    #[test]
    fn ask_adapts_to_choice_form_without_changing_public_models() {
        let mut request = AskRequest::new(
            MessagePrompt::default(),
            vec![Question {
                message: "Pick one".into(),
                predefined_options: vec![OptionItem::new("A", true), OptionItem::new("B", false)],
            }],
            false,
        );
        request.single = true;
        let forms = ChoiceFormView::from_ask(&request, "Submit");
        assert_eq!(forms.len(), 1);
        assert!(forms[0].single);
        assert!(forms[0].options[0].recommended);
        assert_eq!(
            forms[0].input.as_ref().unwrap().visibility,
            ChoiceFormInputVisibility::Always
        );
        assert_eq!(forms[0].options[1].wire_index, 1);
    }

    #[test]
    fn confirm_submission_maps_wire_index_and_limits_comment() {
        let request = confirm_spec()
            .into_request("req-1".into(), 1_000, 2_000)
            .unwrap();

        let approve = request
            .resolve_submission(0, Some("must be discarded".into()), "popup")
            .unwrap();
        assert_eq!(approve.action_id, "approve_once");
        assert_eq!(approve.comment, None);

        let deny = request
            .resolve_submission(1, Some("  unsafe  ".into()), "feishu")
            .unwrap();
        assert_eq!(deny.action_id, "deny");
        assert_eq!(deny.comment.as_deref(), Some("unsafe"));

        assert!(request
            .resolve_submission(2, None, "popup")
            .unwrap_err()
            .contains("out of range"));
        assert!(request
            .resolve_submission(1, Some("x".repeat(1001)), "popup")
            .unwrap_err()
            .contains("exceeds"));
    }

    #[test]
    fn long_task_input_is_required() {
        let mut spec = confirm_spec();
        match &mut spec.presentation {
            ConfirmPresentation::SingleSelectSubmit {
                input,
                default_action_id,
                ..
            } => {
                let input = input.as_mut().unwrap();
                input.visible_when_action_id = "approve_once".into();
                input.max_chars = 3000;
                input.required = true;
                *default_action_id = Some("approve_once".into());
            }
        }
        let request = spec.into_request("task-1".into(), 1, 2).unwrap();
        assert!(request
            .resolve_submission(0, None, "slack")
            .unwrap_err()
            .contains("required"));
        assert_eq!(
            request
                .resolve_submission(0, Some("Do work".into()), "slack")
                .unwrap()
                .comment
                .as_deref(),
            Some("Do work")
        );
    }

    #[test]
    fn always_visible_input_is_only_required_for_its_manual_action() {
        let mut spec = confirm_spec();
        spec.choices.insert(
            1,
            ConfirmChoice {
                id: "todo:1".into(),
                label: "Project TODO".into(),
                description: String::new(),
                role: crate::confirm::ActionRole::Default,
                variant: None,
            },
        );
        match &mut spec.presentation {
            ConfirmPresentation::SingleSelectSubmit {
                input,
                default_action_id,
                ..
            } => {
                let input = input.as_mut().unwrap();
                input.visible_when_action_id = "approve_once".into();
                input.always_visible = true;
                input.required = true;
                input.max_chars = 3000;
                input
                    .prefix_chars_by_action_id
                    .insert("todo:1".into(), 2980);
                *default_action_id = Some("approve_once".into());
            }
        }
        let request = spec.into_request("task-2".into(), 1, 2).unwrap();
        assert_eq!(
            request.choice_form_view().input.unwrap().visibility,
            ChoiceFormInputVisibility::Always
        );
        assert!(request.resolve_submission(0, None, "slack").is_err());
        let todo = request
            .resolve_submission(1, Some("  extra context  ".into()), "slack")
            .unwrap();
        assert_eq!(todo.action_id, "todo:1");
        assert_eq!(todo.comment.as_deref(), Some("extra context"));
        assert!(request.resolve_submission(1, None, "slack").is_ok());
        assert_eq!(request.input_max_chars_for_choice(1), Some(18));
        assert!(request
            .resolve_submission(1, Some("x".repeat(19)), "slack")
            .is_err());
    }

    #[test]
    fn confirm_wire_roundtrip_uses_camel_case() {
        let request = confirm_spec()
            .into_request("req-1".into(), 1_000, 2_000)
            .unwrap();
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains(r#""createdAtMs":1000"#));
        assert!(json.contains(r#""visibleWhenActionId":"deny""#));
        assert!(json.contains(r#""type":"singleSelectSubmit""#));
        let back: ConfirmRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn legacy_long_confirm_input_remains_required() {
        let input: ConfirmInput = serde_json::from_str(
            r#"{"id":"task","visibleWhenActionId":"start","label":"Task","maxChars":3000}"#,
        )
        .unwrap();
        assert!(!input.always_visible);
        assert!(!input.required);
        assert!(input.requires_value());
    }
}
