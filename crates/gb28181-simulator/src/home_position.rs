//! HomePosition 协议矩阵的可复用场景定义与离线报告模型。
//!
//! 这里仅描述 simulator 侧可注入的输入/期望，不携带 UVP 的业务状态机。
//! 网络 example 可以把这些 profile 映射到真实设备会话；无平台实例时，
//! `home_position_matrix` 仍能生成同一份固定 schema 的离线 fixture，避免
//! 用 fake sender 冒充网络验收。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// 当前矩阵报告版本。
pub const MATRIX_SCHEMA_VERSION: u32 = 1;

/// HomePosition v1 的固定场景 ID。顺序是报告与日志的稳定顺序。
pub const HOME_POSITION_SCENARIO_IDS: [&str; 11] = [
    "udp-200-ok-nested",
    "tcp-202-ok-nested",
    "udp-202-error",
    "tcp-200-no-data",
    "udp-delay",
    "tcp-duplicate",
    "udp-reorder",
    "tcp-drop",
    "udp-reload-before-control-ack",
    "tcp-reload-during-reconcile",
    "udp-reload-before-query-response",
];

/// 矩阵使用的传输名称。JSON 中用小写，和 scenario ID 保持一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatrixTransport {
    Udp,
    Tcp,
}

impl MatrixTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
        }
    }
}

/// 查询响应形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueryMode {
    Nested,
    NoData,
}

impl QueryMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nested => "nested",
            Self::NoData => "no-data",
        }
    }
}

/// 只在测试/场景层生效的故障 profile。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultProfile {
    None,
    Delay,
    Duplicate,
    Reorder,
    Drop,
    ReloadBeforeControlAck,
    ReloadDuringReconcile,
    ReloadBeforeQueryResponse,
}

impl FaultProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Delay => "delay",
            Self::Duplicate => "duplicate",
            Self::Reorder => "reorder",
            Self::Drop => "drop",
            Self::ReloadBeforeControlAck => "reload-before-control-ack",
            Self::ReloadDuringReconcile => "reload-during-reconcile",
            Self::ReloadBeforeQueryResponse => "reload-before-query-response",
        }
    }
}

/// 单个矩阵场景及其可审计的期望。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixCase {
    pub id: String,
    pub transport: MatrixTransport,
    #[serde(rename = "sipStatus")]
    pub sip_status: u16,
    pub result: String,
    #[serde(rename = "queryMode")]
    pub query_mode: QueryMode,
    pub fault: FaultProfile,
    #[serde(rename = "operationIds")]
    pub operation_ids: Vec<String>,
    pub assertions: Vec<String>,
    pub pass: bool,
}

/// T13.1 固定报告 schema。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixReport {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "platformSha")]
    pub platform_sha: String,
    #[serde(rename = "simulatorSha")]
    pub simulator_sha: String,
    pub cases: Vec<MatrixCase>,
}

impl MatrixReport {
    /// 构造不依赖外部平台的离线 fixture。fixture 的 operationId 只用于
    /// schema/关联验证，永远不会被当作真实平台操作 ID。
    pub fn offline(platform_sha: impl Into<String>, simulator_sha: impl Into<String>) -> Self {
        Self {
            schema_version: MATRIX_SCHEMA_VERSION,
            platform_sha: non_empty_sha(platform_sha.into()),
            simulator_sha: non_empty_sha(simulator_sha.into()),
            cases: fixed_cases(),
        }
    }

    /// 校验 schema、固定场景集合以及“全通过才能写报告”门禁。
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != MATRIX_SCHEMA_VERSION {
            return Err(format!(
                "不支持的 schemaVersion={}, expected {}",
                self.schema_version, MATRIX_SCHEMA_VERSION
            ));
        }
        if self.platform_sha.trim().is_empty() || self.simulator_sha.trim().is_empty() {
            return Err("platformSha/simulatorSha 不能为空".into());
        }
        let expected: HashSet<&str> = HOME_POSITION_SCENARIO_IDS.into_iter().collect();
        let actual: HashSet<&str> = self.cases.iter().map(|case| case.id.as_str()).collect();
        if actual != expected || self.cases.len() != HOME_POSITION_SCENARIO_IDS.len() {
            return Err(format!(
                "场景集合不匹配: expected={:?}, actual={:?}",
                HOME_POSITION_SCENARIO_IDS, actual
            ));
        }
        for case in &self.cases {
            if case.operation_ids.is_empty() {
                return Err(format!("{} 缺少 operationIds", case.id));
            }
            if case.assertions.is_empty() {
                return Err(format!("{} 缺少 assertions", case.id));
            }
            if !case.pass {
                return Err(format!("{} 未通过，拒绝写入部分报告", case.id));
            }
        }
        Ok(())
    }

    /// 先校验完整报告，再一次性写出，避免 case 失败时留下“部分成功”文件。
    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<(), String> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|err| format!("创建报告目录失败 {}: {err}", parent.display()))?;
        }
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|err| format!("报告序列化失败: {err}"))?;
        fs::write(path, bytes).map_err(|err| format!("写入报告失败 {}: {err}", path.display()))
    }
}

/// 返回稳定顺序的 11 个默认 profile。
pub fn fixed_cases() -> Vec<MatrixCase> {
    HOME_POSITION_SCENARIO_IDS
        .iter()
        .map(|id| case_for(id))
        .collect()
}

fn case_for(id: &str) -> MatrixCase {
    let (transport, sip_status, result, query_mode, fault) = match id {
        "udp-200-ok-nested" => (
            MatrixTransport::Udp,
            200,
            "OK",
            QueryMode::Nested,
            FaultProfile::None,
        ),
        "tcp-202-ok-nested" => (
            MatrixTransport::Tcp,
            202,
            "OK",
            QueryMode::Nested,
            FaultProfile::None,
        ),
        "udp-202-error" => (
            MatrixTransport::Udp,
            202,
            "ERROR",
            QueryMode::Nested,
            FaultProfile::None,
        ),
        "tcp-200-no-data" => (
            MatrixTransport::Tcp,
            200,
            "OK",
            QueryMode::NoData,
            FaultProfile::None,
        ),
        "udp-delay" => (
            MatrixTransport::Udp,
            200,
            "OK",
            QueryMode::Nested,
            FaultProfile::Delay,
        ),
        "tcp-duplicate" => (
            MatrixTransport::Tcp,
            200,
            "OK",
            QueryMode::Nested,
            FaultProfile::Duplicate,
        ),
        "udp-reorder" => (
            MatrixTransport::Udp,
            200,
            "OK",
            QueryMode::Nested,
            FaultProfile::Reorder,
        ),
        "tcp-drop" => (
            MatrixTransport::Tcp,
            200,
            "TIMEOUT",
            QueryMode::Nested,
            FaultProfile::Drop,
        ),
        "udp-reload-before-control-ack" => (
            MatrixTransport::Udp,
            200,
            "OK",
            QueryMode::Nested,
            FaultProfile::ReloadBeforeControlAck,
        ),
        "tcp-reload-during-reconcile" => (
            MatrixTransport::Tcp,
            200,
            "OK",
            QueryMode::Nested,
            FaultProfile::ReloadDuringReconcile,
        ),
        "udp-reload-before-query-response" => (
            MatrixTransport::Udp,
            200,
            "OK",
            QueryMode::Nested,
            FaultProfile::ReloadBeforeQueryResponse,
        ),
        _ => unreachable!("unknown fixed scenario: {id}"),
    };

    MatrixCase {
        id: id.into(),
        transport,
        sip_status,
        result: result.into(),
        query_mode,
        fault,
        operation_ids: vec![
            format!("fixture:{id}:control"),
            format!("fixture:{id}:query"),
        ],
        assertions: assertions_for(id),
        pass: true,
    }
}

fn assertions_for(id: &str) -> Vec<String> {
    let mut assertions: Vec<String> = vec![
        "call-id-cseq-and-sn-correlated".into(),
        "operation-id-is-linked".into(),
    ];
    match id {
        "udp-200-ok-nested" | "tcp-202-ok-nested" => {
            assertions.extend(
                [
                    "2xx-reaches-sent",
                    "device-control-ok-accepted",
                    "nested-home-position",
                ]
                .into_iter()
                .map(String::from),
            );
        }
        "udp-202-error" => {
            assertions.extend(
                ["2xx-reaches-sent", "device-control-error-rejected"]
                    .into_iter()
                    .map(String::from),
            );
        }
        "tcp-200-no-data" => assertions.push("no-data-does-not-overwrite-known-value".into()),
        "udp-delay" => assertions.push("deadline-and-timeout-are-bounded".into()),
        "tcp-duplicate" => assertions.push("duplicate-ack-is-idempotent".into()),
        "udp-reorder" => assertions.push("causal-ordering-prevents-stale-write".into()),
        "tcp-drop" => assertions.push("dropped-response-reaches-unknown".into()),
        "udp-reload-before-control-ack" => {
            assertions.push("reload-resumes-control-operation".into())
        }
        "tcp-reload-during-reconcile" => {
            assertions.push("reload-resumes-reconcile-without-duplicate-control".into())
        }
        "udp-reload-before-query-response" => {
            assertions.push("reload-resumes-query-operation".into())
        }
        _ => unreachable!("unknown fixed scenario: {id}"),
    }
    assertions
}

fn non_empty_sha(value: String) -> String {
    if value.trim().is_empty() {
        "unknown".into()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn home_position_fault_profile() {
        let cases = fixed_cases();
        assert_eq!(cases.len(), 11);
        assert_eq!(
            cases
                .iter()
                .map(|case| case.id.as_str())
                .collect::<Vec<_>>(),
            HOME_POSITION_SCENARIO_IDS
        );
        assert_eq!(cases[0].transport, MatrixTransport::Udp);
        assert_eq!(cases[1].sip_status, 202);
        assert_eq!(cases[2].result, "ERROR");
        assert_eq!(cases[3].query_mode, QueryMode::NoData);
        assert_eq!(cases[4].fault, FaultProfile::Delay);
        assert_eq!(cases[5].fault, FaultProfile::Duplicate);
        assert_eq!(cases[6].fault, FaultProfile::Reorder);
        assert_eq!(cases[7].fault, FaultProfile::Drop);
        assert_eq!(cases[8].fault, FaultProfile::ReloadBeforeControlAck);
        assert_eq!(cases[9].fault, FaultProfile::ReloadDuringReconcile);
        assert_eq!(cases[10].fault, FaultProfile::ReloadBeforeQueryResponse);
    }

    #[test]
    fn report_schema_rejects_failed_case_without_writing_partial_file() {
        let mut report = MatrixReport::offline("platform", "simulator");
        report.cases[0].pass = false;
        let path = std::env::temp_dir().join(format!(
            "home-position-matrix-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        assert!(report.write_json(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn report_schema_uses_camel_case_wire_fields() {
        let report = MatrixReport::offline("platform", "simulator");
        let value = serde_json::to_value(report).expect("json");
        assert_eq!(value["schemaVersion"], 1);
        assert!(value["platformSha"].is_string());
        assert!(value["simulatorSha"].is_string());
        assert_eq!(value["cases"][0]["sipStatus"], 200);
        assert_eq!(value["cases"][0]["queryMode"], "nested");
        assert!(value["cases"][0]["operationIds"].is_array());
    }
}
