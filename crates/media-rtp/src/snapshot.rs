//! 本地抓拍图库。
//!
//! 抓拍来源是预览总线上的最新 JPEG 帧：那一帧就是平台正在看到的画面，不再单独
//! 触发一次采集。因此抓拍不会打断推流，也不需要额外开一次摄像头/屏幕采集。
//!
//! 落盘策略：一帧一个文件，文件名即稳定 ID（`snap-YYYYMMDD-hhmmss-<captured_at_ms>`）。
//! 列表按文件名里的 captured_at_ms 倒序，不依赖文件系统返回顺序。

use serde::Serialize;
use std::path::{Path, PathBuf};

/// 抓拍元数据（桌面端图库直接消费）。
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotMeta {
    /// 稳定 ID，等于文件名去扩展名。
    pub id: String,
    pub file_name: String,
    pub path: String,
    pub byte_len: u64,
    /// 帧被切出的时刻（毫秒纪元），来自预览总线。
    pub captured_at_ms: i64,
    /// 落盘时刻（毫秒纪元）。二者不同：抓拍拿到的可能是稍早的一帧。
    pub saved_at_ms: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// 磁盘上的抓拍目录。
pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 落盘一帧 JPEG，返回元数据。
    pub fn save(&self, jpeg: &[u8], captured_at_ms: i64) -> Result<SnapshotMeta, String> {
        if jpeg.is_empty() {
            return Err("抓拍帧为空".into());
        }
        let saved_at_ms = common::clock::local_epoch_ms();
        let stamp = file_stamp(captured_at_ms.max(0));
        // captured_at_ms 已能区分帧，但同一毫秒内重复点击仍可能撞名，加后缀兜底。
        let mut attempt = 0u32;
        let (id, path) = loop {
            let id = if attempt == 0 {
                format!("snap-{stamp}-{captured_at_ms}")
            } else {
                format!("snap-{stamp}-{captured_at_ms}-{attempt}")
            };
            let path = self.root.join(format!("{id}.jpg"));
            if !path.exists() {
                break (id, path);
            }
            attempt += 1;
            if attempt > 99 {
                return Err("抓拍文件名冲突，无法落盘".into());
            }
        };
        std::fs::write(&path, jpeg)
            .map_err(|error| format!("写入抓拍文件 {} 失败: {error}", path.display()))?;
        let (width, height) = match jpeg_dimensions(jpeg) {
            Some((width, height)) => (Some(width), Some(height)),
            None => (None, None),
        };
        Ok(SnapshotMeta {
            id,
            file_name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_string_lossy().into_owned(),
            byte_len: jpeg.len() as u64,
            captured_at_ms,
            saved_at_ms,
            width,
            height,
        })
    }

    /// 按时间倒序列出最近 `limit` 张。
    pub fn list(&self, limit: usize) -> Result<Vec<SnapshotMeta>, String> {
        let entries = std::fs::read_dir(&self.root)
            .map_err(|error| format!("读取抓拍目录 {} 失败: {error}", self.root.display()))?;
        let mut metas = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jpg") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            // 文件名尾部的 captured_at_ms 是排序与时间展示的唯一来源；
            // 解析不出来说明不是本程序写的文件，直接跳过。
            let Some(captured_at_ms) = id.rsplit('-').next().and_then(|tail| tail.parse().ok())
            else {
                continue;
            };
            // 列表要显示尺寸，但逐张读完整 JPEG 代价高；只读文件头即可，
            // SOF 段在这个窗口内（实测 MJPEG 帧的 SOF 在前 1 KiB 内）。
            let (width, height) = read_dimensions(&path).unwrap_or((None, None));
            metas.push(SnapshotMeta {
                id: id.to_string(),
                file_name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: path.to_string_lossy().into_owned(),
                byte_len: meta.len(),
                captured_at_ms,
                saved_at_ms: meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0),
                width,
                height,
            });
        }
        metas.sort_by(|a, b| {
            b.captured_at_ms
                .cmp(&a.captured_at_ms)
                .then_with(|| b.id.cmp(&a.id))
        });
        metas.truncate(limit);
        Ok(metas)
    }

    pub fn read(&self, id: &str) -> Result<Vec<u8>, String> {
        let path = self.path_of(id)?;
        std::fs::read(&path).map_err(|error| format!("读取抓拍 {} 失败: {error}", path.display()))
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let path = self.path_of(id)?;
        std::fs::remove_file(&path)
            .map_err(|error| format!("删除抓拍 {} 失败: {error}", path.display()))
    }

    /// 复制到用户选定路径（另存为）。
    pub fn export_copy(&self, id: &str, target: &Path) -> Result<(), String> {
        let source = self.path_of(id)?;
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("创建目录 {} 失败: {error}", parent.display()))?;
            }
        }
        std::fs::copy(&source, target).map_err(|error| {
            format!(
                "复制抓拍 {} → {} 失败: {error}",
                source.display(),
                target.display()
            )
        })?;
        Ok(())
    }

    /// ID → 路径。只接受单段文件名字符，杜绝 `../` 这类越界读取。
    fn path_of(&self, id: &str) -> Result<PathBuf, String> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(format!("非法抓拍 ID: {id}"));
        }
        Ok(self.root.join(format!("{id}.jpg")))
    }
}

/// 文件名片段：`YYYYMMDD-hhmmss`（按校时后的本地挂钟，便于人工翻目录）。
fn file_stamp(epoch_ms: i64) -> String {
    let iso = common::clock::epoch_to_iso8601(epoch_ms.div_euclid(1_000));
    iso.replace('-', "").replace(':', "").replace('T', "-")
}

fn read_dimensions(path: &Path) -> Option<(Option<u32>, Option<u32>)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = vec![0u8; 64 * 1024];
    let read = file.read(&mut head).ok()?;
    let (width, height) = jpeg_dimensions(&head[..read])?;
    Some((Some(width), Some(height)))
}

/// 从 JPEG 字节流里取宽高（SOF0~SOF15，排除 DHT/JPG/DAC 三个非 SOF 标记）。
pub fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut index = 2;
    while index + 3 < data.len() {
        if data[index] != 0xFF {
            index += 1;
            continue;
        }
        let marker = data[index + 1];
        // 填充字节 0xFF 连续出现时继续往后找。
        if marker == 0xFF {
            index += 1;
            continue;
        }
        // 无负载的标记。
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            index += 2;
            continue;
        }
        let length = u16::from_be_bytes([data[index + 2], data[index + 3]]) as usize;
        let is_sof = (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4
            && marker != 0xC8
            && marker != 0xCC;
        if is_sof {
            if index + 9 > data.len() {
                return None;
            }
            let height = u16::from_be_bytes([data[index + 5], data[index + 6]]) as u32;
            let width = u16::from_be_bytes([data[index + 7], data[index + 8]]) as u32;
            if width == 0 || height == 0 {
                return None;
            }
            return Some((width, height));
        }
        // 进入扫描数据后就不会再有 SOF 了（正常 JPEG 的 SOF 一定在 SOS 之前）。
        if marker == 0xDA {
            return None;
        }
        index += 2 + length;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小编码的 JPEG 头：SOI + SOF0(width,height) + EOI。
    fn fake_jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        out.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&[0x03]);
        out.extend_from_slice(&[0u8; 9]);
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    /// 测试目录放在 crate 的 target 下，而不是 `%TEMP%`。
    ///
    /// 这台开发机装了企业 DLP 客户端，`%TEMP%` 下 `create_dir_all` 成功但写文件被拒
    /// （os error 5），工程所在的 D 盘可写。用 manifest 目录可以绕开这个环境陷阱，
    /// 让测试本身不依赖机器策略。
    fn temp_dir(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-snapshots");
        let dir = root.join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn parses_sof_dimensions() {
        assert_eq!(jpeg_dimensions(&fake_jpeg(640, 480)), Some((640, 480)));
        assert_eq!(jpeg_dimensions(&fake_jpeg(1920, 1080)), Some((1920, 1080)));
    }

    #[test]
    fn rejects_non_jpeg_and_truncated() {
        assert_eq!(jpeg_dimensions(b"not-a-jpeg"), None);
        assert_eq!(jpeg_dimensions(&[0xFF, 0xD8]), None);
        // 只有 SOI + SOF 头的一半，不能猜尺寸。
        assert_eq!(jpeg_dimensions(&[0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11]), None);
    }

    /// DHT(0xC4) 紧跟在 SOI 后面时不能被误判成 SOF。
    #[test]
    fn skips_dht_before_sof() {
        let mut data = vec![0xFF, 0xD8];
        data.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x05, 0x00, 0x00, 0x00]);
        data.extend_from_slice(&fake_jpeg(320, 240)[2..]);
        assert_eq!(jpeg_dimensions(&data), Some((320, 240)));
    }

    #[test]
    fn file_stamp_is_sortable_and_filesystem_safe() {
        let stamp = file_stamp(1_767_000_000_000);
        assert_eq!(stamp.len(), 15, "YYYYMMDD-hhmmss 固定 15 字符");
        assert!(!stamp.contains(':') && !stamp.contains('T'), "实际: {stamp}");
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let dir = temp_dir("traversal");
        let store = SnapshotStore::open(&dir).expect("创建临时抓拍目录");
        for bad in ["../secret", "a/b", "", "a.b", "中文", "a b"] {
            assert!(store.read(bad).is_err(), "应拒绝非法 ID: {bad}");
            assert!(store.delete(bad).is_err(), "应拒绝非法 ID: {bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saves_lists_and_deletes() {
        let dir = temp_dir("save");
        let store = SnapshotStore::open(&dir).expect("创建临时抓拍目录");
        let first = store
            .save(&fake_jpeg(640, 480), 1_767_000_000_000)
            .expect("落盘");
        let second = store
            .save(&fake_jpeg(1280, 720), 1_767_000_001_000)
            .expect("落盘");
        assert_eq!(first.width, Some(640));
        assert_eq!(second.height, Some(720));

        let list = store.list(10).expect("列目录");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second.id, "最新的排最前");
        assert_eq!(list[1].id, first.id);
        // 列表里的尺寸来自只读文件头解析，不能是 None。
        assert_eq!(list[0].width, Some(1280));

        let bytes = store.read(&first.id).expect("读取");
        assert_eq!(bytes.len(), first.byte_len as usize);

        store.delete(&first.id).expect("删除");
        assert_eq!(store.list(10).expect("列目录").len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_millisecond_capture_does_not_overwrite() {
        let dir = temp_dir("collision");
        let store = SnapshotStore::open(&dir).expect("创建临时抓拍目录");
        let first = store.save(&fake_jpeg(64, 64), 1_767_000_002_000).expect("落盘");
        let second = store.save(&fake_jpeg(64, 64), 1_767_000_002_000).expect("落盘");
        assert_ne!(first.id, second.id, "同毫秒两次抓拍不能互相覆盖");
        assert_eq!(store.list(10).expect("列目录").len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_copy_writes_target_file() {
        let dir = temp_dir("export");
        let store = SnapshotStore::open(&dir).expect("创建临时抓拍目录");
        let meta = store.save(&fake_jpeg(800, 600), 1_767_000_003_000).expect("落盘");
        let target = dir.join("nested").join("copy.jpg");
        store.export_copy(&meta.id, &target).expect("另存为");
        assert!(target.exists());
        assert_eq!(std::fs::metadata(&target).expect("stat").len(), meta.byte_len);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
