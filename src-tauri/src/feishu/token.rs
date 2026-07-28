//! tenant_access_token 获取与进程内缓存。
//!
//! `POST {base}/open-apis/auth/v3/tenant_access_token/internal {app_id, app_secret}`
//! → `tenant_access_token`（有效期 `expire` 秒，约 7200）。所有 OpenAPI 调用 header 携带
//! `Authorization: Bearer <tenant_access_token>`。

use super::FeishuError;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    base_url: String,
    app_id: String,
    secret_sha256: [u8; 32],
}

struct Cached {
    token: String,
    expire_at: Instant,
}

static CACHE: OnceLock<Mutex<HashMap<CacheKey, Cached>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<CacheKey, Cached>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(base_url: &str, app_id: &str, app_secret: &str) -> CacheKey {
    let base_url = match base_url.trim().trim_end_matches('/') {
        "" => "https://open.feishu.cn".to_string(),
        value => value.to_string(),
    };
    CacheKey {
        base_url,
        app_id: app_id.trim().to_string(),
        secret_sha256: Sha256::digest(app_secret.trim().as_bytes()).into(),
    }
}

fn cached_token(key: &CacheKey) -> Option<String> {
    let mut guard = cache().lock().ok()?;
    let cached = guard.get(key)?;
    if cached.expire_at > Instant::now() {
        Some(cached.token.clone())
    } else {
        guard.remove(key);
        None
    }
}

/// Remove the token associated with a configuration that is no longer active.
pub fn invalidate_credentials(base_url: &str, app_id: &str, app_secret: &str) {
    if let Ok(mut guard) = cache().lock() {
        guard.remove(&cache_key(base_url, app_id, app_secret));
    }
}

/// Remove a rejected token only if it is still the cached value. A concurrent request may already
/// have refreshed the entry; in that case the fresh token must remain available.
pub fn invalidate_if_matches(
    base_url: &str,
    app_id: &str,
    app_secret: &str,
    rejected_token: &str,
) -> bool {
    let key = cache_key(base_url, app_id, app_secret);
    let Ok(mut guard) = cache().lock() else {
        return false;
    };
    if guard
        .get(&key)
        .is_some_and(|cached| cached.token == rejected_token)
    {
        guard.remove(&key);
        true
    } else {
        false
    }
}

/// 取 tenant_access_token：命中未过期缓存直接返回，否则换取并缓存（过期前留 60s 余量）。
/// 缓存键包含服务域名、`app_id` 与 App Secret 的 SHA-256 指纹，凭据轮换不会复用旧 token。
pub async fn get_token(
    http: &reqwest::Client,
    base_url: &str,
    app_id: &str,
    app_secret: &str,
) -> Result<String, FeishuError> {
    let key = cache_key(base_url, app_id, app_secret);
    if let Some(token) = cached_token(&key) {
        return Ok(token);
    }

    let url = format!(
        "{}/open-apis/auth/v3/tenant_access_token/internal",
        key.base_url
    );
    let resp = http
        .post(&url)
        .json(&serde_json::json!({ "app_id": app_id, "app_secret": app_secret }))
        .send()
        .await
        .map_err(|e| FeishuError::Network(e.to_string()))?;
    let http_status = resp.status().as_u16();
    let log_id = super::response_log_id(resp.headers());
    let body: Value = resp
        .json()
        .await
        .map_err(|_| FeishuError::bad_response_with_metadata(Some(http_status), log_id.clone()))?;
    // 飞书业务码：code==0 成功；否则取 msg。
    if body.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = body
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("failed to obtain tenant_access_token (check AppId/AppSecret)")
            .to_string();
        return Err(FeishuError::api_response(
            body.get("code").and_then(|code| code.as_i64()),
            msg,
            Some(http_status),
            log_id,
        ));
    }
    let token = body
        .get("tenant_access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FeishuError::bad_response_with_metadata(Some(http_status), log_id.clone()))?
        .to_string();
    let expire = body.get("expire").and_then(|v| v.as_u64()).unwrap_or(7200);
    let expire_at = Instant::now() + Duration::from_secs(expire.saturating_sub(60));

    if let Ok(mut guard) = cache().lock() {
        guard.insert(
            key,
            Cached {
                token: token.clone(),
                expire_at,
            },
        );
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(key: CacheKey, token: &str) {
        cache().lock().unwrap().insert(
            key,
            Cached {
                token: token.to_string(),
                expire_at: Instant::now() + Duration::from_secs(60),
            },
        );
    }

    #[test]
    fn cache_key_separates_base_url_and_secret_rotations() {
        let old = cache_key("https://open.feishu.cn/", "app-key-test", "secret-old");
        let new_secret = cache_key("https://open.feishu.cn", "app-key-test", "secret-new");
        let lark = cache_key("https://open.larksuite.com", "app-key-test", "secret-old");
        insert(old.clone(), "old-token");

        assert_eq!(cached_token(&old).as_deref(), Some("old-token"));
        assert_eq!(cached_token(&new_secret), None);
        assert_eq!(cached_token(&lark), None);
    }

    #[test]
    fn conditional_invalidation_does_not_remove_a_refreshed_token() {
        let key = cache_key("https://token-race.test", "app-race-test", "secret");
        insert(key.clone(), "old-token");
        assert!(invalidate_if_matches(
            &key.base_url,
            &key.app_id,
            "secret",
            "old-token"
        ));

        insert(key.clone(), "fresh-token");
        assert!(!invalidate_if_matches(
            &key.base_url,
            &key.app_id,
            "secret",
            "old-token"
        ));
        assert_eq!(cached_token(&key).as_deref(), Some("fresh-token"));
    }

    #[test]
    fn invalidating_changed_credentials_removes_the_old_entry() {
        let key = cache_key("https://old-config.test", "app-config-test", "secret");
        insert(key.clone(), "old-token");

        invalidate_credentials(&key.base_url, &key.app_id, "secret");

        assert_eq!(cached_token(&key), None);
    }
}
