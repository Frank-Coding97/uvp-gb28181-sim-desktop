use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraLeaseOwner {
    Device,
    Demo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct CameraLeaseGrant {
    pub token: u64,
    pub owner: CameraLeaseOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CameraLeaseError {
    pub code: String,
    pub message: String,
    pub owner: Option<CameraLeaseOwner>,
}

pub struct CameraLease {
    next_token: AtomicU64,
    current: Mutex<Option<CameraLeaseGrant>>,
}

impl CameraLease {
    pub fn new() -> Self {
        Self {
            next_token: AtomicU64::new(1),
            current: Mutex::new(None),
        }
    }

    pub fn acquire(&self, owner: CameraLeaseOwner) -> Result<CameraLeaseGrant, CameraLeaseError> {
        let mut current = self.current.lock().map_err(|_| CameraLeaseError {
            code: "camera_lease_unavailable".into(),
            message: "摄像头租约状态不可用，请重启应用后重试".into(),
            owner: None,
        })?;
        if let Some(active) = *current {
            let holder = match active.owner {
                CameraLeaseOwner::Device => "已注册设备的视频采集",
                CameraLeaseOwner::Demo => "视频采集 Demo",
            };
            return Err(CameraLeaseError {
                code: "camera_in_use".into(),
                message: format!("摄像头正被{holder}占用，请先停止它再重试"),
                owner: Some(active.owner),
            });
        }
        let grant = CameraLeaseGrant {
            token: self.next_token.fetch_add(1, Ordering::Relaxed).max(1),
            owner,
        };
        *current = Some(grant);
        Ok(grant)
    }

    pub fn release(&self, token: u64) -> bool {
        let Ok(mut current) = self.current.lock() else {
            return false;
        };
        if current.as_ref().is_some_and(|grant| grant.token == token) {
            *current = None;
            true
        } else {
            false
        }
    }

    pub fn current(&self) -> Option<CameraLeaseGrant> {
        self.current.lock().ok().and_then(|current| *current)
    }
}

impl Default for CameraLease {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device与demo不能同时持有摄像头() {
        let lease = CameraLease::new();
        let device = lease.acquire(CameraLeaseOwner::Device).unwrap();
        let error = lease.acquire(CameraLeaseOwner::Demo).unwrap_err();
        assert_eq!(error.code, "camera_in_use");
        assert_eq!(error.owner, Some(CameraLeaseOwner::Device));
        assert!(lease.release(device.token));
    }

    #[test]
    fn 旧token不能释放后来获得的租约() {
        let lease = CameraLease::new();
        let first = lease.acquire(CameraLeaseOwner::Demo).unwrap();
        assert!(lease.release(first.token));
        let second = lease.acquire(CameraLeaseOwner::Demo).unwrap();

        assert!(!lease.release(first.token));
        assert_eq!(lease.current().unwrap().token, second.token);
        assert!(lease.release(second.token));
        assert!(lease.current().is_none());
    }
}
