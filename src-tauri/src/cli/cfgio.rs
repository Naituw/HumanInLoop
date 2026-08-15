//! CLI 配置命令公共工具：点号路径读写、类型强制、密钥识别与取值（env/file/stdin/隐藏输入）、
//! 脱敏、交互输入与异步运行时助手。仅供 `cli/{config_cmd,channel_cmd,agents_cmd,doctor}` 复用。

use crate::config::AppConfig;
use crate::i18n::Lang;
use crate::secrets;
use serde_json::Value;
#[cfg(unix)]
use std::io::BufRead;
use std::io::{IsTerminal, Read, Write};

/// 本地化：按语言选英 / 中。CLI 配置命令专属文案用它（既有错误仍走 `i18n::tr`）。
pub fn t(lang: Lang, en: &str, zh: &str) -> String {
    match lang {
        Lang::Zh => zh.to_string(),
        Lang::En => en.to_string(),
    }
}

/// 5 个密钥键（点号 camelCase，与 `secrets` 账户常量一致）。
pub const SECRET_KEYS: [&str; 5] = [
    secrets::ACCOUNT_DINGTALK_SECRET,
    secrets::ACCOUNT_FEISHU_SECRET,
    secrets::ACCOUNT_TELEGRAM_TOKEN,
    secrets::ACCOUNT_SLACK_BOT_TOKEN,
    secrets::ACCOUNT_SLACK_APP_TOKEN,
];

/// 该点号键是否为密钥键（值只进钥匙串、不落 config.json、不进 argv）。
pub fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS.contains(&key)
}

/// 创建一次性当前线程 tokio 运行时并 block_on（与 `client::run_ask` 同款）。
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(fut)
}

/// Return daemon status on every supported desktop platform.
pub fn daemon_status() -> Option<crate::ipc::StatusInfo> {
    block_on(crate::client::request_status())
}

// ——— 点号路径读写（基于 serde_json::Value，camelCase）———

/// 取点号路径的引用（任一段缺失返回 None）。
pub fn get_path<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    let mut cur = root;
    for seg in key.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// 写点号路径；中间 / 末段缺失即报「未知键」（配置 schema 固定，不自动建节点）。
pub fn set_path(root: &mut Value, key: &str, val: Value) -> Result<(), String> {
    let segs: Vec<&str> = key.split('.').collect();
    let mut cur = root;
    for seg in &segs[..segs.len() - 1] {
        cur = cur.get_mut(*seg).ok_or_else(|| unknown_key(key))?;
    }
    let last = segs[segs.len() - 1];
    let obj = cur.as_object_mut().ok_or_else(|| unknown_key(key))?;
    if !obj.contains_key(last) {
        return Err(unknown_key(key));
    }
    obj.insert(last.to_string(), val);
    Ok(())
}

fn unknown_key(key: &str) -> String {
    format!("unknown config key: {key}")
}

/// 把字符串输入按「该键现有值的 JSON 类型」转成 `Value`（无 schema 的类型化写入）。
pub fn coerce_to_type(existing: &Value, input: &str) -> Result<Value, String> {
    match existing {
        Value::Bool(_) => parse_bool(input).map(Value::Bool),
        Value::Number(n) => {
            if n.is_f64() && !n.is_i64() && !n.is_u64() {
                input
                    .trim()
                    .parse::<f64>()
                    .map(Value::from)
                    .map_err(|_| format!("expected a number, got: {input}"))
            } else {
                input
                    .trim()
                    .parse::<i64>()
                    .map(Value::from)
                    .map_err(|_| format!("expected an integer, got: {input}"))
            }
        }
        Value::String(_) => Ok(Value::String(input.to_string())),
        _ => Err("this key is not a simple scalar and cannot be set via the CLI".to_string()),
    }
}

/// 宽松布尔解析。
pub fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "y" => Ok(true),
        "false" | "0" | "no" | "off" | "n" => Ok(false),
        _ => Err(format!("expected a boolean (true/false), got: {s}")),
    }
}

// ——— 脱敏（show / get 永不打印真实密钥）———

/// 取「不含密钥真值」的配置 `Value`，并把各密钥键替换为 `●●●`（已设）/ 空串（未设）。
pub fn redacted_value() -> Value {
    let cfg = AppConfig::load_without_secrets();
    let mut v = serde_json::to_value(&cfg).unwrap_or(Value::Null);
    for key in SECRET_KEYS {
        let mark = if secret_is_set(key) { "●●●" } else { "" };
        let _ = set_path(&mut v, key, Value::String(mark.to_string()));
    }
    v
}

/// 密钥是否已设置（钥匙串有，或旧明文回退仍在磁盘配置里）。
pub fn secret_is_set(account: &str) -> bool {
    if secrets::has(account) {
        return true;
    }
    // 回退：钥匙串不可用时密钥以明文存于 config.json。
    let cfg = AppConfig::load_without_secrets();
    let v = serde_json::to_value(&cfg).unwrap_or(Value::Null);
    get_path(&v, account)
        .and_then(|x| x.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

// ——— 密钥取值来源（脚本安全：不进 argv）———

/// 密钥输入来源；互斥其一。
pub enum SecretSource {
    /// 从环境变量读取（`--<field>-env <VAR>`）。
    Env(String),
    /// 从文件读取并 trim（`--<field>-file <path>`，支持 `~/`）。
    File(String),
    /// 从标准输入读取（`--<field>-stdin` 或值 `-`）。
    Stdin,
    /// 交互式隐藏输入（终端，无上述来源时）。
    Prompt(String),
}

/// 按来源读取密钥（统一 trim 尾随空白 / 换行）。
pub fn read_secret(src: &SecretSource, lang: Lang) -> Result<String, String> {
    let raw = match src {
        SecretSource::Env(var) => std::env::var(var).map_err(|_| {
            t(
                lang,
                &format!("environment variable {var} is not set"),
                &format!("环境变量 {var} 未设置"),
            )
        })?,
        SecretSource::File(path) => {
            std::fs::read_to_string(expand_tilde(path)).map_err(|e| format!("{path}: {e}"))?
        }
        SecretSource::Stdin => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| e.to_string())?;
            s
        }
        SecretSource::Prompt(label) => prompt_hidden(label)?,
    };
    Ok(raw.trim().to_string())
}

/// Expand a leading tilde using the platform home-directory provider.
pub fn expand_tilde(p: &str) -> std::path::PathBuf {
    crate::cli::file_attachment::expand_tilde(p, &crate::paths::home())
}

/// stdin 是否为终端（决定走交互向导还是报错要求 flag）。
pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

// ——— 交互输入 ———

/// 普通行输入（显示当前值，回车保留）。提示走 stderr，保持 stdout 洁净。
pub fn prompt_line(label: &str, current: &str) -> Result<String, String> {
    eprint!("{label}");
    if !current.is_empty() {
        eprint!(" [{current}]");
    }
    eprint!(": ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let v = line.trim_end_matches(['\n', '\r']).to_string();
    Ok(if v.is_empty() { current.to_string() } else { v })
}

/// Hidden secret input on Unix terminals.
#[cfg(unix)]
pub fn prompt_hidden(label: &str) -> Result<String, String> {
    use std::os::unix::io::AsRawFd;
    eprint!("{label}: ");
    std::io::stderr().flush().ok();
    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let have_tty = unsafe { libc::tcgetattr(fd, &mut term) } == 0;
    let saved = term;
    if have_tty {
        term.c_lflag &= !libc::ECHO;
        unsafe {
            libc::tcsetattr(fd, libc::TCSANOW, &term);
        }
    }
    let mut line = String::new();
    let res = stdin.lock().read_line(&mut line);
    if have_tty {
        unsafe {
            libc::tcsetattr(fd, libc::TCSANOW, &saved);
        }
        eprintln!();
    }
    res.map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

#[cfg(windows)]
struct WindowsConsoleModeGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
    original: u32,
    restored: bool,
}

#[cfg(windows)]
impl WindowsConsoleModeGuard {
    fn restore(&mut self) -> std::io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let ok = unsafe {
            windows_sys::Win32::System::Console::SetConsoleMode(self.handle, self.original)
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        self.restored = true;
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsConsoleModeGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(windows)]
enum SecretInputUnit {
    Continue,
    Complete,
    Cancelled,
    Eof,
}

#[cfg(windows)]
fn consume_secret_input_unit(buffer: &mut Vec<u16>, unit: u16) -> SecretInputUnit {
    match unit {
        0x0d | 0x0a => SecretInputUnit::Complete,
        0x03 => SecretInputUnit::Cancelled,
        0x1a => SecretInputUnit::Eof,
        0x08 => {
            if buffer
                .pop()
                .is_some_and(|last| (0xdc00..=0xdfff).contains(&last))
                && buffer
                    .last()
                    .is_some_and(|first| (0xd800..=0xdbff).contains(first))
            {
                buffer.pop();
            }
            SecretInputUnit::Continue
        }
        0 => SecretInputUnit::Continue,
        _ => {
            buffer.push(unit);
            SecretInputUnit::Continue
        }
    }
}

/// Hidden secret input using the Windows console directly. Processed input is disabled while
/// reading so Ctrl+C is returned as a character and the mode guard can restore every changed bit.
#[cfg(windows)]
pub fn prompt_hidden(label: &str) -> Result<String, String> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, ReadConsoleW, SetConsoleMode, ENABLE_ECHO_INPUT,
        ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, STD_INPUT_HANDLE,
    };

    if !stdin_is_tty() {
        return Err(
            "hidden secret input requires an interactive console; use --from-env, --from-file, or --from-stdin"
                .to_string(),
        );
    }
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut original = 0u32;
    if unsafe { GetConsoleMode(handle, &mut original) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let hidden = original & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT);
    if unsafe { SetConsoleMode(handle, hidden) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut guard = WindowsConsoleModeGuard {
        handle,
        original,
        restored: false,
    };

    eprint!("{label}: ");
    std::io::stderr().flush().ok();
    let mut units = Vec::new();
    let result = loop {
        let mut unit = 0u16;
        let mut read = 0u32;
        if unsafe {
            ReadConsoleW(
                handle,
                (&mut unit as *mut u16).cast(),
                1,
                &mut read,
                std::ptr::null(),
            )
        } == 0
        {
            break Err(std::io::Error::last_os_error().to_string());
        }
        if read == 0 {
            break Err("secret input reached EOF".to_string());
        }
        match consume_secret_input_unit(&mut units, unit) {
            SecretInputUnit::Continue => {}
            SecretInputUnit::Complete => {
                break String::from_utf16(&units).map_err(|e| e.to_string())
            }
            SecretInputUnit::Cancelled => break Err("secret input cancelled".to_string()),
            SecretInputUnit::Eof => break Err("secret input reached EOF".to_string()),
        }
    };
    let restored = guard.restore().map_err(|e| e.to_string());
    eprintln!();
    restored?;
    result
}

#[cfg(not(any(unix, windows)))]
pub fn prompt_hidden(_label: &str) -> Result<String, String> {
    Err("hidden secret input is unsupported on this platform".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_parse_variants() {
        assert!(parse_bool("true").unwrap());
        assert!(parse_bool("YES").unwrap());
        assert!(!parse_bool("off").unwrap());
        assert!(parse_bool("nope").is_err());
    }

    #[test]
    fn path_get_set_on_config() {
        let cfg = AppConfig::default();
        let mut v = serde_json::to_value(&cfg).unwrap();
        assert_eq!(
            get_path(&v, "channels.telegram.apiBaseUrl")
                .unwrap()
                .as_str(),
            Some("https://api.telegram.org")
        );
        set_path(&mut v, "channels.autoActivation", Value::Bool(true)).unwrap();
        assert_eq!(
            get_path(&v, "channels.autoActivation").unwrap().as_bool(),
            Some(true)
        );
        assert!(set_path(&mut v, "channels.nope", Value::Bool(true)).is_err());
    }

    #[test]
    fn coerce_uses_existing_type() {
        assert_eq!(
            coerce_to_type(&Value::Bool(false), "true").unwrap(),
            Value::Bool(true)
        );
        assert!(coerce_to_type(&Value::from(1i64), "abc").is_err());
        assert_eq!(
            coerce_to_type(&Value::String(String::new()), "hi").unwrap(),
            Value::String("hi".to_string())
        );
    }

    #[test]
    fn secret_keys_match_accounts() {
        assert!(is_secret_key("channels.telegram.botToken"));
        assert!(is_secret_key("channels.slack.appToken"));
        assert!(!is_secret_key("channels.telegram.chatId"));
    }

    #[test]
    fn config_paths_expand_all_home_forms() {
        let home = crate::paths::home();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/secret.txt"), home.join("secret.txt"));
        assert_eq!(expand_tilde("~\\secret.txt"), home.join("secret.txt"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_secret_units_handle_unicode_edit_cancel_and_eof() {
        let mut value = Vec::new();
        for unit in "密🔑".encode_utf16() {
            assert!(matches!(
                consume_secret_input_unit(&mut value, unit),
                SecretInputUnit::Continue
            ));
        }
        assert!(matches!(
            consume_secret_input_unit(&mut value, 0x08),
            SecretInputUnit::Continue
        ));
        assert_eq!(String::from_utf16(&value).unwrap(), "密");
        assert!(matches!(
            consume_secret_input_unit(&mut value, 0x03),
            SecretInputUnit::Cancelled
        ));
        assert!(matches!(
            consume_secret_input_unit(&mut value, 0x1a),
            SecretInputUnit::Eof
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_secret_console_smoke_when_requested() {
        use windows_sys::Win32::System::Console::{GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE};

        let Ok(expected) = std::env::var("ASKHUMAN_WINDOWS_SECRET_SMOKE") else {
            return;
        };
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut before = 0u32;
        assert_ne!(unsafe { GetConsoleMode(handle, &mut before) }, 0);
        let result = prompt_hidden("secret-smoke");
        let mut after = 0u32;
        assert_ne!(unsafe { GetConsoleMode(handle, &mut after) }, 0);
        assert_eq!(after, before, "console mode must always be restored");
        if expected == "<cancel>" {
            assert_eq!(result.unwrap_err(), "secret input cancelled");
        } else {
            assert_eq!(result.unwrap(), expected);
        }
    }
}
