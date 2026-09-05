//! 平台凭据存储。
//!
//! 设计要点(安全):
//! - 密码**只**写入 OS 钥匙串(macOS Keychain / Windows Credential Manager / libsecret),
//!   前端 localStorage 里只保留「该档案是否已配置密码」这个布尔。
//! - 密码**永不**回传 webview:`start_device` / `start_stress` 只接收 `profile_id`,
//!   由 Rust 侧现取现用。
//! - 钥匙串不可用时(无桌面会话的 Linux、被策略禁用等)退化为**仅本进程内存**,
//!   宁可重启后要求重新输入,也不落任何磁盘明文。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::ipc_error::{IpcError, IpcResult};

/// 钥匙串中的服务名(与包标识一致,便于用户在钥匙串工具里辨认)。
const SERVICE: &str = "uvp-gb28181-desktop";

/// 实际生效的存储后端,用于向用户说明持久化程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// 已写入 OS 钥匙串,重启后仍在。
    Keyring,
    /// 钥匙串不可用,仅保存在本进程内存中,退出即失效。
    Memory,
}

impl Backend {
    pub fn describe(self) -> &'static str {
        match self {
            Backend::Keyring => "已保存到系统钥匙串",
            Backend::Memory => "系统钥匙串不可用,密码仅在本次运行期间有效",
        }
    }
}

/// 钥匙串不可用时的会话级兜底存储。
fn session_store() -> &'static Mutex<HashMap<String, String>> {
    static STORE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 档案 ID 会成为钥匙串条目的 account 名,限制字符集避免歧义与注入式命名。
fn validate_profile_id(profile_id: &str) -> Result<(), String> {
    if profile_id.is_empty() || profile_id.len() > 64 {
        return Err("平台档案标识长度必须在 1..=64 之间".into());
    }
    if !profile_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("平台档案标识只能包含字母、数字、- 和 _".into());
    }
    Ok(())
}

fn entry(profile_id: &str) -> Option<keyring::Entry> {
    match keyring::Entry::new(SERVICE, profile_id) {
        Ok(entry) => Some(entry),
        Err(error) => {
            tracing::warn!(%error, "钥匙串条目创建失败,退化为会话内存存储");
            None
        }
    }
}

/// 保存密码。返回实际生效的后端,供 UI 告知用户持久化程度。
pub fn set(profile_id: &str, password: &str) -> Result<Backend, String> {
    validate_profile_id(profile_id)?;
    if password.is_empty() {
        return Err("SIP 认证密码不能为空".into());
    }
    if let Some(entry) = entry(profile_id) {
        match entry.set_password(password) {
            Ok(()) => {
                // 钥匙串写入成功后清掉可能残留的内存副本,保持单一来源。
                if let Ok(mut store) = session_store().lock() {
                    store.remove(profile_id);
                }
                return Ok(Backend::Keyring);
            }
            Err(error) => {
                tracing::warn!(%error, "写入钥匙串失败,退化为会话内存存储");
            }
        }
    }
    session_store()
        .lock()
        .map_err(|_| "凭据存储状态已损坏,请重启应用".to_string())?
        .insert(profile_id.to_string(), password.to_string());
    Ok(Backend::Memory)
}

/// 取出密码。未配置时返回可直接展示给用户的提示。
pub fn get(profile_id: &str) -> Result<String, String> {
    validate_profile_id(profile_id)?;
    if let Some(entry) = entry(profile_id) {
        match entry.get_password() {
            Ok(password) if !password.is_empty() => return Ok(password),
            Ok(_) => {}
            Err(keyring::Error::NoEntry) => {}
            Err(error) => {
                tracing::warn!(%error, "读取钥匙串失败,回退查会话内存");
            }
        }
    }
    let cached = session_store()
        .lock()
        .map_err(|_| "凭据存储状态已损坏,请重启应用".to_string())?
        .get(profile_id)
        .cloned();
    cached.ok_or_else(|| "该平台档案尚未配置密码,请在顶栏平台设置中填写并保存".to_string())
}

/// 删除密码(删除档案时调用)。条目本就不存在时视为成功。
pub fn remove(profile_id: &str) -> Result<(), String> {
    validate_profile_id(profile_id)?;
    if let Some(entry) = entry(profile_id) {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(error) => tracing::warn!(%error, "删除钥匙串条目失败"),
        }
    }
    if let Ok(mut store) = session_store().lock() {
        store.remove(profile_id);
    }
    Ok(())
}

/// 是否已配置密码(前端用于渲染「已配置 / 未配置」而不需要拿到明文)。
pub fn has(profile_id: &str) -> bool {
    get(profile_id).is_ok()
}

// ── IPC 命令 ─────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct SecretSaveResult {
    backend: Backend,
    message: String,
}

fn credential_error(message: String) -> IpcError {
    if message.contains("尚未配置密码") {
        IpcError::new("credential.missing", message)
            .with_retryable(false)
            .with_action("请在平台设置中填写并保存 SIP 密码")
    } else if message.contains("密码不能为空") {
        IpcError::new("credential.invalid", message).with_retryable(false)
    } else if message.contains("档案标识") {
        IpcError::config(message)
    } else {
        IpcError::new("credential.store", message).with_retryable(true)
    }
}

/// 保存平台档案密码到 OS 钥匙串。
#[tauri::command]
pub fn save_platform_secret(profile_id: String, password: String) -> IpcResult<SecretSaveResult> {
    let backend = set(&profile_id, &password).map_err(credential_error)?;
    Ok(SecretSaveResult {
        backend,
        message: backend.describe().to_string(),
    })
}

/// 删除平台档案密码。
#[tauri::command]
pub fn delete_platform_secret(profile_id: String) -> IpcResult<()> {
    remove(&profile_id).map_err(credential_error)
}

/// 批量查询哪些档案已配置密码(一次 IPC 渲染整个列表的状态)。
#[tauri::command]
pub fn platform_secret_status(profile_ids: Vec<String>) -> HashMap<String, bool> {
    profile_ids
        .into_iter()
        .map(|id| {
            let ok = has(&id);
            (id, ok)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 档案标识字符集校验() {
        assert!(validate_profile_id("p-1700000000").is_ok());
        assert!(validate_profile_id("local_A9").is_ok());
        assert!(validate_profile_id("").is_err());
        assert!(validate_profile_id("bad id").is_err());
        assert!(validate_profile_id("bad/../id").is_err());
        assert!(validate_profile_id(&"x".repeat(65)).is_err());
    }

    #[test]
    fn 凭据命令错误使用稳定代码() {
        assert_eq!(
            credential_error("该平台档案尚未配置密码".into()).code,
            "credential.missing"
        );
        assert_eq!(
            credential_error("SIP 认证密码不能为空".into()).code,
            "credential.invalid"
        );
        assert_eq!(
            credential_error("平台档案标识只能包含字母".into()).code,
            "config.invalid"
        );
    }

    #[test]
    fn 空密码被拒绝() {
        assert!(set("unit-test-empty", "").is_err());
    }

    #[test]
    fn 会话兜底可读回() {
        // 不依赖 CI 环境是否有可用钥匙串:直接写内存兜底再读回。
        session_store()
            .lock()
            .unwrap()
            .insert("unit-test-mem".into(), "s3cret".into());
        assert_eq!(get("unit-test-mem").unwrap(), "s3cret");
        remove("unit-test-mem").unwrap();
        assert!(get("unit-test-mem").is_err());
    }
}
