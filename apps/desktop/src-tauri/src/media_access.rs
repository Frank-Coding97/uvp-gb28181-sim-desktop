//! 媒体文件预览的授权闸门。
//!
//! # 这个模块存在的原因
//!
//! `assetProtocol.scope` 被刻意收窄到视频目录与应用自有临时目录,用户从任意路径选的
//! 视频文件需要在运行时逐个放行。之前的做法是给 webview 开一个
//! `allow_media_preview(path)` 命令,只检查 `path.is_file()` 就放行 ——
//! **这等于把收窄后的 scope 又整体打开了**:任何能在 webview 里执行 JS 的途径
//! (被污染的前端依赖、把不可信内容渲染成 HTML 的疏忽)都可以
//! `invoke("allow_media_preview", { path: "/Users/x/.ssh/id_rsa" })`,
//! 然后用 `asset://` 把文件内容读出来。
//!
//! 现在的规则是:**只有 Rust 侧自己弹出的系统文件对话框能产生授权**。
//! 授权记录持久化在应用数据目录,于是重启后用户上次选的文件仍能预览,
//! 而 webview 无论传什么路径都只能命中已授权集合里的条目。
//!
//! 额外两道闸(纵深防御,防止将来有人改坏上面那条主规则):
//! 1. 扩展名必须在媒体白名单里 —— 授权集合里不可能出现 `.ssh` 或 `.env`;
//! 2. 路径一律先 `canonicalize()` 再比较 —— 符号链接换不出别的目标。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 可预览的媒体扩展名。与前端文件选择器的 filters 保持一致。
///
/// 裸 H.264/H.265 浏览器放不出来(前端会提示),但仍列在这里 ——
/// 用户选了它就该算"选过",不然换文件后预览提示会前后矛盾。
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "h264", "264", "h265", "hevc", "mp4", "flv", "mkv", "mov", "m4v", "ts",
];

/// 授权集合的上限。超出后按先进先出淘汰。
///
/// 不设上限会让这个文件随使用无限增长;64 条足够覆盖"最近用过的素材",
/// 而淘汰掉的条目用户重新选一次即可恢复。
const MAX_GRANTS: usize = 64;

/// 授权记录的文件名(位于应用数据目录)。
const GRANTS_FILE: &str = "media-grants.json";

/// 用户经系统文件对话框授权过的媒体文件集合。
pub struct MediaGrants {
    /// 按授权顺序排列,便于超限时淘汰最早的一条。
    granted: Mutex<VecDeque<PathBuf>>,
    /// 持久化位置;为 None 表示拿不到应用数据目录,退化为仅本次运行有效。
    store: Option<PathBuf>,
}

impl MediaGrants {
    /// 从应用数据目录载入已有授权。
    ///
    /// 文件损坏时**从空集合开始**而不是报错:授权记录是缓存性质的,
    /// 丢了只是让用户重新选一次文件,没有理由因此让应用起不来。
    pub fn load(app_data_dir: Option<PathBuf>) -> Self {
        let store = app_data_dir.map(|dir| dir.join(GRANTS_FILE));
        let granted = store
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
            .map(|list| {
                list.into_iter()
                    .map(PathBuf::from)
                    // 载入时就丢掉已不存在或扩展名不合规的条目:
                    // 手工编辑过这个文件的人不该因此拿到额外能力。
                    .filter(|p| p.is_file() && has_media_extension(p))
                    .take(MAX_GRANTS)
                    .collect::<VecDeque<_>>()
            })
            .unwrap_or_default();
        MediaGrants {
            granted: Mutex::new(granted),
            store,
        }
    }

    /// 记录一条授权。仅供"Rust 侧对话框返回了这个路径"这一条路径调用。
    ///
    /// 返回规范化后的路径(写进 asset scope 与回给前端的都该是它)。
    pub fn grant(&self, path: &Path) -> Result<PathBuf, String> {
        let canonical = canonicalize_media(path)?;
        {
            let mut granted = self.granted.lock().map_err(|_| POISONED)?;
            if !granted.iter().any(|p| p == &canonical) {
                granted.push_back(canonical.clone());
                while granted.len() > MAX_GRANTS {
                    granted.pop_front();
                }
            }
        }
        self.persist();
        Ok(canonical)
    }

    /// 判断一个(来自 webview 的)路径是否已被授权。
    pub fn is_granted(&self, path: &Path) -> bool {
        match canonicalize_media(path) {
            Ok(canonical) => self
                .granted
                .lock()
                .map(|granted| granted.iter().any(|p| p == &canonical))
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    /// 已授权的条目数(诊断用)。
    pub fn len(&self) -> usize {
        self.granted.lock().map(|g| g.len()).unwrap_or(0)
    }

    fn persist(&self) {
        let Some(store) = &self.store else { return };
        let Ok(granted) = self.granted.lock() else {
            return;
        };
        let list: Vec<String> = granted
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let Ok(text) = serde_json::to_string_pretty(&list) else {
            return;
        };
        if let Some(parent) = store.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 写失败只是"下次启动要重新选文件",不该打断当前操作。
        if let Err(error) = std::fs::write(store, text) {
            tracing::debug!(%error, "媒体预览授权记录写入失败(仅影响下次启动)");
        }
    }
}

const POISONED: &str = "媒体授权状态已损坏,请重启应用";

/// 扩展名是否在媒体白名单里(大小写不敏感)。
pub fn has_media_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.as_str()))
}

/// 规范化并校验一个媒体路径:必须是**已存在的普通文件**且扩展名合规。
///
/// `canonicalize` 会解析掉符号链接与 `..`,所以比较的是真实目标 ——
/// 否则可以先授权一个 `a.mp4`,再把它改成指向别处的符号链接。
fn canonicalize_media(path: &Path) -> Result<PathBuf, String> {
    if !has_media_extension(path) {
        return Err(format!(
            "只能预览媒体文件({})",
            MEDIA_EXTENSIONS.join(" / ")
        ));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("文件不可访问: {e}"))?;
    if !canonical.is_file() {
        return Err("路径不是一个普通文件".into());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uvp-media-grants-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"fake").expect("写文件");
        p
    }

    #[test]
    fn 未授权的路径一律拒绝() {
        let dir = temp_dir("deny");
        let grants = MediaGrants::load(Some(dir.join("state")));
        let video = touch(&dir, "a.mp4");
        assert!(
            !grants.is_granted(&video),
            "没经过对话框的路径不该被放行,哪怕它确实是个视频文件"
        );
        assert_eq!(grants.len(), 0);
    }

    #[test]
    fn 非媒体扩展名不可授权() {
        let dir = temp_dir("ext");
        let grants = MediaGrants::load(Some(dir.join("state")));
        // 这是本模块存在的核心理由:webview 拿到任意路径读取能力等于绕过 asset scope。
        let key = touch(&dir, "id_rsa");
        let err = grants.grant(&key).expect_err("私钥不该能被授权");
        assert!(err.contains("只能预览媒体文件"), "实际: {err}");
        assert!(!grants.is_granted(&key));
        // 伪装成 .mp4.txt 同样不行。
        let fake = touch(&dir, "movie.mp4.txt");
        assert!(grants.grant(&fake).is_err());
    }

    #[test]
    fn 授权后可通过且大小写扩展名都认() {
        let dir = temp_dir("allow");
        let grants = MediaGrants::load(Some(dir.join("state")));
        let video = touch(&dir, "b.MP4");
        let canonical = grants.grant(&video).expect("应能授权");
        assert!(grants.is_granted(&video));
        assert!(grants.is_granted(&canonical));
        // 同一路径重复授权不重复计数。
        grants.grant(&video).expect("重复授权应成功");
        assert_eq!(grants.len(), 1);
    }

    #[test]
    fn 不存在的文件不可授权() {
        let dir = temp_dir("missing");
        let grants = MediaGrants::load(Some(dir.join("state")));
        let err = grants
            .grant(&dir.join("nope.mp4"))
            .expect_err("不存在的文件不该能授权");
        assert!(err.contains("不可访问"), "实际: {err}");
    }

    #[test]
    fn 超过上限后淘汰最早的授权() {
        let dir = temp_dir("cap");
        let grants = MediaGrants::load(Some(dir.join("state")));
        let first = touch(&dir, "v0.mp4");
        grants.grant(&first).expect("授权");
        for i in 1..=MAX_GRANTS {
            let p = touch(&dir, &format!("v{i}.mp4"));
            grants.grant(&p).expect("授权");
        }
        assert_eq!(grants.len(), MAX_GRANTS);
        assert!(
            !grants.is_granted(&first),
            "最早的一条应被淘汰,否则这个文件会无限增长"
        );
    }

    #[test]
    fn 授权跨重启保留且损坏文件回落空集合() {
        let dir = temp_dir("persist");
        let state = dir.join("state");
        let video = touch(&dir, "keep.mp4");
        {
            let grants = MediaGrants::load(Some(state.clone()));
            grants.grant(&video).expect("授权");
        }
        // 重新载入:上次选过的文件仍可预览(否则每次重启都要重新选一遍)。
        let reloaded = MediaGrants::load(Some(state.clone()));
        assert!(reloaded.is_granted(&video));

        // 记录文件损坏 → 从空集合开始,而不是让应用起不来。
        std::fs::write(state.join(GRANTS_FILE), b"{ not json").expect("写坏文件");
        let broken = MediaGrants::load(Some(state.clone()));
        assert_eq!(broken.len(), 0);

        // 手工往记录里塞非媒体路径,载入时应被过滤掉。
        let key = touch(&dir, "secret");
        let forged = serde_json::to_string(&[key.to_string_lossy()]).expect("序列化");
        std::fs::write(state.join(GRANTS_FILE), forged).expect("写伪造记录");
        let filtered = MediaGrants::load(Some(state));
        assert_eq!(filtered.len(), 0, "手工编辑授权文件不该换来额外的读取能力");
    }

    #[test]
    fn 拿不到数据目录时退化为仅本次运行有效() {
        let dir = temp_dir("nostore");
        let grants = MediaGrants::load(None);
        let video = touch(&dir, "c.mp4");
        grants.grant(&video).expect("仍应能在内存里授权");
        assert!(grants.is_granted(&video));
    }
}
