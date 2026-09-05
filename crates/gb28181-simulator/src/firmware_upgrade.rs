//! GB/T 28181-2022 设备固件升级的模拟实现。
//!
//! 国标只规定控制请求、业务应答和最终结果通知，下载包格式及模拟设备如何落盘
//! 属于本模拟器的工程约定。这里把约定收拢到一个小模块，设备主状态机只负责
//! 调度 SIP 消息和重新注册。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use common::{Error, GbVersion, Result};
use gb28181_protocol::manscdp::{Control, DeviceUpgrade};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 模拟器测试固件包的格式标识。
pub(crate) const PACKAGE_FORMAT: &str = "uvp-simulator-firmware-v1";
/// 单个下载响应的工程上限，避免错误 URL 把模拟器内存耗尽。
pub(crate) const MAX_PACKAGE_BYTES: usize = 8 * 1024 * 1024;
/// 固件下载（含响应体读取）的总超时。
pub(crate) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// 一次升级请求经过协议和设备属性校验后的不可变快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedUpgrade {
    pub(crate) device_id: String,
    pub(crate) firmware: String,
    pub(crate) file_url: String,
    pub(crate) manufacturer: String,
    pub(crate) session_id: String,
    pub(crate) fingerprint: String,
}

/// 设备主动升级的调度结果。
#[derive(Debug, Clone)]
pub(crate) enum UpgradeDispatch {
    /// 新会话，持久化后执行一次。
    Start(ValidatedUpgrade),
    /// 已完成会话，只重放最终结果，不再下载或刷写。
    Replay {
        session_id: String,
        session: StoredSession,
    },
    /// 同一进程内已有任务执行，业务应答仍回 OK，但不重复启动任务。
    AlreadyRunning,
}

/// 升级结果的内部表示，与国标 Notify 的 OK/ERROR 映射一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpgradeResult {
    pub(crate) firmware: String,
    pub(crate) success: bool,
    pub(crate) failure_reason: Option<String>,
}

/// 下载阶段错误，用于映射 A.2.5.9 的失败原因码。
#[derive(Debug)]
pub(crate) enum DownloadError {
    Timeout(String),
    TooLarge(String),
    Other(String),
}

impl DownloadError {
    pub(crate) fn reason_code(&self) -> &'static str {
        match self {
            DownloadError::Timeout(_) => "01",
            DownloadError::TooLarge(_) => "02",
            DownloadError::Other(_) => "99",
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            DownloadError::Timeout(message)
            | DownloadError::TooLarge(message)
            | DownloadError::Other(message) => message,
        }
    }
}

/// 持久化在设备专属 JSON 文件中的升级会话状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredSession {
    /// 归一化请求参数的 SHA-256，不把完整 URL 或下载内容写入状态文件。
    pub(crate) fingerprint: String,
    pub(crate) target_firmware: String,
    pub(crate) status: SessionStatus,
    pub(crate) current_firmware: String,
    #[serde(default)]
    pub(crate) failure_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionStatus {
    InProgress,
    Success,
    Error,
}

impl StoredSession {
    pub(crate) fn result(&self) -> UpgradeResult {
        UpgradeResult {
            firmware: self.current_firmware.clone(),
            success: self.status == SessionStatus::Success,
            failure_reason: self.failure_reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredState {
    schema_version: u32,
    current_firmware: String,
    #[serde(default)]
    sessions: BTreeMap<String, StoredSession>,
}

impl StoredState {
    fn initial(firmware: String) -> Self {
        Self {
            schema_version: 1,
            current_firmware: firmware,
            sessions: BTreeMap::new(),
        }
    }
}

/// 设备级升级持久化存储。每个设备实例使用一个文件，写入采用同目录临时文件 + rename。
pub(crate) struct UpgradeStore {
    path: Option<PathBuf>,
    state: Mutex<StoredState>,
    /// 状态文件存在但不可读/不可写时锁死升级，避免伪装成新设备重新执行。
    unavailable: Mutex<Option<String>>,
}

impl UpgradeStore {
    pub(crate) fn open(
        state_dir: Option<PathBuf>,
        device_id: &str,
        initial_firmware: &str,
    ) -> Self {
        let path = state_dir.map(|dir| dir.join(format!("{device_id}.json")));
        let mut unavailable = None;
        let mut state = StoredState::initial(initial_firmware.to_owned());
        if let Some(path) = path.as_deref() {
            match Self::load(path) {
                Ok(Some(loaded)) => state = loaded,
                Ok(None) => {}
                Err(reason) => {
                    tracing::warn!(path = %path.display(), %reason, "固件升级状态不可用，拒绝后续升级");
                    unavailable = Some(reason);
                }
            }
        }

        // 进程在下载/应用期间崩溃时，磁盘上只会留下 InProgress。恢复成明确失败，
        // 避免下一次启动把它误报成成功，也不会自动再次下载或刷写。
        let mut interrupted = false;
        for session in state.sessions.values_mut() {
            if session.status == SessionStatus::InProgress {
                session.status = SessionStatus::Error;
                session.current_firmware = state.current_firmware.clone();
                session.failure_reason = Some("03".into());
                interrupted = true;
            }
        }
        let store = Self {
            path,
            state: Mutex::new(state),
            unavailable: Mutex::new(unavailable),
        };
        if interrupted {
            if let Err(error) = store.persist() {
                tracing::warn!(%error, "固件升级中断状态持久化失败");
                store.mark_unavailable(error.to_string());
            }
        }
        store
    }

    fn load(path: &Path) -> std::result::Result<Option<StoredState>, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!("读取状态文件失败: {error}"));
            }
        };
        match serde_json::from_slice::<StoredState>(&bytes) {
            Ok(state) if state.schema_version == 1 && !state.current_firmware.is_empty() => {
                Ok(Some(state))
            }
            Ok(_) => Err("状态文件 schema_version 不支持或 current_firmware 为空".into()),
            Err(error) => Err(format!("解析状态文件失败: {error}")),
        }
    }

    pub(crate) fn current_firmware(&self) -> String {
        if self.unavailable_reason().is_some() {
            // 状态文件损坏时不能把构造参数当成磁盘真相返回，否则 DeviceInfo
            // 会伪造一个回退版本。升级入口也会因同一标记拒绝新会话。
            return "unknown".into();
        }
        self.state
            .lock()
            .map(|state| state.current_firmware.clone())
            .unwrap_or_default()
    }

    pub(crate) fn lookup(&self, session_id: &str) -> Option<StoredSession> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.sessions.get(session_id).cloned())
    }

    /// 写入 InProgress 记录。失败时恢复内存快照，调用方不得开始下载。
    pub(crate) fn begin(&self, upgrade: &ValidatedUpgrade) -> Result<()> {
        if let Some(reason) = self.unavailable_reason() {
            return Err(Error::Config(format!("固件升级状态不可用: {reason}")));
        }
        self.mutate(|state| {
            state.sessions.insert(
                upgrade.session_id.clone(),
                StoredSession {
                    fingerprint: upgrade.fingerprint.clone(),
                    target_firmware: upgrade.firmware.clone(),
                    status: SessionStatus::InProgress,
                    current_firmware: state.current_firmware.clone(),
                    failure_reason: None,
                },
            );
        })
    }

    /// 将会话和当前固件版本在同一个状态文件中原子提交。
    pub(crate) fn complete(&self, session_id: &str, result: &UpgradeResult) -> Result<()> {
        self.mutate(|state| {
            let Some(session) = state.sessions.get_mut(session_id) else {
                return;
            };
            if result.success {
                state.current_firmware = result.firmware.clone();
                session.status = SessionStatus::Success;
                session.current_firmware = result.firmware.clone();
                session.failure_reason = None;
            } else {
                session.status = SessionStatus::Error;
                session.current_firmware = state.current_firmware.clone();
                session.failure_reason = result.failure_reason.clone();
            }
        })
    }

    fn mutate(&self, f: impl FnOnce(&mut StoredState)) -> Result<()> {
        if let Some(reason) = self.unavailable_reason() {
            return Err(Error::Config(format!("固件升级状态不可用: {reason}")));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Gb28181("固件升级状态锁异常".into()))?;
        let before = state.clone();
        f(&mut state);
        if let Err(error) = self.persist_state(&state) {
            *state = before;
            self.mark_unavailable(error.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn persist(&self) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::Gb28181("固件升级状态锁异常".into()))?;
        self.persist_state(&state)
    }

    fn unavailable_reason(&self) -> Option<String> {
        self.unavailable.lock().ok().and_then(|value| value.clone())
    }

    fn mark_unavailable(&self, reason: String) {
        if let Ok(mut value) = self.unavailable.lock() {
            *value = Some(reason);
        }
    }

    fn persist_state(&self, state: &StoredState) -> Result<()> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| Error::Config("固件升级状态目录非法".into()))?;
        fs::create_dir_all(parent).map_err(Error::Io)?;
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| Error::Gb28181(format!("固件升级状态序列化失败: {error}")))?;
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        fs::write(&tmp, bytes).map_err(Error::Io)?;
        fs::rename(&tmp, path).map_err(|error| {
            let _ = fs::remove_file(&tmp);
            Error::Io(error)
        })
    }
}

/// 根据请求字段生成去重指纹。内容只用于比较，不把 URL 明文保存到磁盘。
pub(crate) fn request_fingerprint(device_id: &str, upgrade: &DeviceUpgrade) -> String {
    let mut digest = Sha256::new();
    for value in [
        device_id,
        upgrade.firmware.trim(),
        upgrade.file_url.trim(),
        upgrade.manufacturer.as_deref().unwrap_or_default().trim(),
    ] {
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    hex_encode(&digest.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// 校验国标版本、目标设备、厂商、字段必填项和 SessionID 约束，并解析 URL。
pub(crate) fn validate_request(
    version: &GbVersion,
    device_id: &str,
    manufacturer: &str,
    ctrl: &Control,
) -> std::result::Result<ValidatedUpgrade, String> {
    if !version.is_2022() {
        return Err("设备当前按 GB/T 28181-2016 运行，不接受 DeviceUpgrade".into());
    }
    if ctrl.cmd_type != "DeviceControl" {
        return Err(format!(
            "DeviceUpgrade 只能出现在 CmdType=DeviceControl 中，实际为 {}",
            ctrl.cmd_type
        ));
    }
    if ctrl.device_id != device_id {
        return Err(format!("DeviceID 不匹配: {}", ctrl.device_id));
    }
    let upgrade = ctrl
        .device_upgrade
        .as_ref()
        .ok_or_else(|| "缺少 DeviceUpgrade 子命令".to_string())?;
    let firmware = upgrade.firmware.trim();
    if firmware.is_empty() {
        return Err("Firmware 不能为空".into());
    }
    let file_url = upgrade.file_url.trim();
    if file_url.is_empty() {
        return Err("FileURL 不能为空".into());
    }
    let request_manufacturer = upgrade
        .manufacturer
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Manufacturer 不能为空".to_string())?;
    if request_manufacturer != manufacturer.trim() {
        return Err(format!("Manufacturer 不匹配: {request_manufacturer}"));
    }
    let session_id = upgrade.session_id.trim();
    if !(32..=128).contains(&session_id.len())
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("SessionID 必须是 32-128 字节的 ASCII 字母、数字或短划线".into());
    }
    let url = reqwest::Url::parse(file_url).map_err(|error| format!("FileURL 非法: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("FileURL 只允许 http 或 https".into());
    }
    if url.host_str().is_none() {
        return Err("FileURL 缺少主机".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("FileURL 不允许携带 userinfo".into());
    }

    let normalized = DeviceUpgrade {
        firmware: firmware.to_owned(),
        file_url: file_url.to_owned(),
        manufacturer: Some(request_manufacturer.to_owned()),
        session_id: session_id.to_owned(),
    };
    let fingerprint = request_fingerprint(device_id, &normalized);
    Ok(ValidatedUpgrade {
        device_id: device_id.to_owned(),
        firmware: normalized.firmware,
        file_url: normalized.file_url,
        manufacturer: request_manufacturer.to_owned(),
        session_id: normalized.session_id,
        fingerprint,
    })
}

/// 下载固件包。客户端关闭自动重定向；任何 3xx 都作为错误返回，避免跳到未授权主机。
pub(crate) async fn download(url: &str) -> std::result::Result<Vec<u8>, DownloadError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| DownloadError::Other(format!("FileURL 解析失败: {error}")))?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|error| DownloadError::Other(format!("下载客户端初始化失败: {error}")))?;
    let mut response = client.get(parsed).send().await.map_err(|error| {
        if error.is_timeout() {
            DownloadError::Timeout(format!("固件下载超时: {error}"))
        } else {
            DownloadError::Other(format!("固件下载失败: {error}"))
        }
    })?;
    let status = response.status();
    if status.is_redirection() {
        return Err(DownloadError::Other(format!(
            "固件下载拒绝未受控重定向: HTTP {status}"
        )));
    }
    if !status.is_success() {
        return Err(DownloadError::Other(format!(
            "固件服务器返回 HTTP {status}"
        )));
    }
    if let Some(length) = response.content_length() {
        if length > MAX_PACKAGE_BYTES as u64 {
            return Err(DownloadError::TooLarge(format!(
                "固件包超过 {} 字节上限",
                MAX_PACKAGE_BYTES
            )));
        }
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            DownloadError::Timeout(format!("固件下载读取超时: {error}"))
        } else {
            DownloadError::Other(format!("固件下载读取失败: {error}"))
        }
    })? {
        if body.len().saturating_add(chunk.len()) > MAX_PACKAGE_BYTES {
            return Err(DownloadError::TooLarge(format!(
                "固件包超过 {} 字节上限",
                MAX_PACKAGE_BYTES
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, Deserialize)]
struct FirmwarePackage {
    format: String,
    manufacturer: String,
    model: String,
    firmware: String,
    payload: String,
    sha256: String,
}

/// 校验本模拟器工程约定的 JSON 测试包，返回包内目标版本。
pub(crate) fn verify_package(
    bytes: &[u8],
    expected_manufacturer: &str,
    expected_model: &str,
    expected_firmware: &str,
) -> std::result::Result<String, String> {
    let package: FirmwarePackage = serde_json::from_slice(bytes)
        .map_err(|error| format!("固件包不是有效 UTF-8 JSON: {error}"))?;
    if package.format != PACKAGE_FORMAT {
        return Err(format!("固件包 format 不支持: {}", package.format));
    }
    if package.manufacturer != expected_manufacturer {
        return Err("固件包 manufacturer 与设备不匹配".into());
    }
    if package.model != expected_model {
        return Err("固件包 model 与设备不匹配".into());
    }
    if package.firmware != expected_firmware {
        return Err("固件包 firmware 与升级请求不匹配".into());
    }
    let mut digest = Sha256::new();
    digest.update(package.payload.as_bytes());
    let actual = hex_encode(&digest.finalize());
    if package.sha256.len() != 64 || !package.sha256.eq_ignore_ascii_case(&actual) {
        return Err("固件包 payload sha256 校验失败".into());
    }
    Ok(package.firmware)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::GbVersion;

    fn upgrade() -> DeviceUpgrade {
        DeviceUpgrade {
            firmware: "0.2.0".into(),
            file_url: "http://127.0.0.1:8080/firmware.json".into(),
            manufacturer: Some("UVP".into()),
            session_id: "a1b2c3d4-a1b2-c3d4-a1b2-c3d4a1b2c3d4".into(),
        }
    }

    fn control(upgrade: DeviceUpgrade) -> Control {
        Control {
            cmd_type: "DeviceControl".into(),
            sn: 1,
            device_id: "34020000001320000001".into(),
            device_upgrade: Some(upgrade),
            ..Default::default()
        }
    }

    #[test]
    fn request_validation_enforces_2022_and_identity() {
        let ctrl = control(upgrade());
        assert!(validate_request(&GbVersion::V2022, "34020000001320000001", "UVP", &ctrl).is_ok());
        assert!(
            validate_request(&GbVersion::V2016, "34020000001320000001", "UVP", &ctrl)
                .unwrap_err()
                .contains("2016")
        );

        let mut wrong = upgrade();
        wrong.file_url = "file:///tmp/firmware.json".into();
        assert!(validate_request(
            &GbVersion::V2022,
            "34020000001320000001",
            "UVP",
            &control(wrong)
        )
        .is_err());
        let mut userinfo = upgrade();
        userinfo.file_url = "http://u:p@127.0.0.1/firmware.json".into();
        assert!(validate_request(
            &GbVersion::V2022,
            "34020000001320000001",
            "UVP",
            &control(userinfo)
        )
        .unwrap_err()
        .contains("userinfo"));
    }

    #[test]
    fn package_hash_and_state_are_deterministic() {
        let payload = "fixture-payload";
        let mut digest = Sha256::new();
        digest.update(payload.as_bytes());
        let package = serde_json::json!({
            "format": PACKAGE_FORMAT,
            "manufacturer": "UVP",
            "model": "Desktop-Sim",
            "firmware": "0.2.0",
            "payload": payload,
            "sha256": hex_encode(&digest.finalize()),
        });
        let bytes = serde_json::to_vec(&package).unwrap();
        assert_eq!(
            verify_package(&bytes, "UVP", "Desktop-Sim", "0.2.0").unwrap(),
            "0.2.0"
        );
        let mut bad = package;
        bad["sha256"] = serde_json::Value::String("0".repeat(64));
        assert!(verify_package(
            &serde_json::to_vec(&bad).unwrap(),
            "UVP",
            "Desktop-Sim",
            "0.2.0"
        )
        .is_err());
    }

    #[test]
    fn interrupted_session_recovers_as_error() {
        let dir = std::env::temp_dir().join(format!(
            "uvp-sim-upgrade-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let store = UpgradeStore::open(Some(dir.clone()), "34020000001320000001", "0.1.0");
        let validated = validate_request(
            &GbVersion::V2022,
            "34020000001320000001",
            "UVP",
            &control(upgrade()),
        )
        .unwrap();
        store.begin(&validated).unwrap();
        drop(store);
        let recovered = UpgradeStore::open(Some(dir.clone()), "34020000001320000001", "0.1.0");
        let session = recovered.lookup(&validated.session_id).unwrap();
        assert_eq!(session.status, SessionStatus::Error);
        assert_eq!(session.failure_reason.as_deref(), Some("03"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_state_fails_closed_without_initial_version_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "uvp-sim-upgrade-corrupt-state-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("34020000001320000001.json");
        fs::write(
            &path,
            br#"{"schema_version":999,"current_firmware":"0.2.0"}"#,
        )
        .unwrap();

        let store = UpgradeStore::open(Some(dir.clone()), "34020000001320000001", "0.1.0");
        assert_eq!(store.current_firmware(), "unknown");
        let validated = validate_request(
            &GbVersion::V2022,
            "34020000001320000001",
            "UVP",
            &control(upgrade()),
        )
        .unwrap();
        assert!(store.begin(&validated).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
