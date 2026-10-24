use std::sync::Arc;

use common::Result;
use sip_core::SipTransport;

use super::*;

/// 设备初始内置的五个具名预置位(可被平台的预置位设置/删除命令修改)。
pub(crate) fn default_presets() -> std::collections::BTreeMap<u8, String> {
    [
        (1, "大门入口"),
        (2, "停车场"),
        (3, "接待大厅"),
        (4, "东侧通道"),
        (5, "西侧通道"),
    ]
    .into_iter()
    .map(|(id, name)| (id, name.to_string()))
    .collect()
}

impl DeviceSimulator {
    /// 控制命令的详细人类可读摘要(供 UI 命令时间线):方向/变倍/预置位/巡航/辅助等。
    pub(crate) fn control_detail(ctrl: &gb28181_protocol::manscdp::Control) -> String {
        use gb28181_protocol::manscdp::{CruiseOp, PresetAction};
        // PTZ 方向/变倍。
        if let Some(m) = ctrl.ptz_motion() {
            if !m.is_stop() {
                let mut parts = Vec::new();
                if m.up {
                    parts.push("上");
                }
                if m.down {
                    parts.push("下");
                }
                if m.left {
                    parts.push("左");
                }
                if m.right {
                    parts.push("右");
                }
                if m.zoom_in {
                    parts.push("放大");
                }
                if m.zoom_out {
                    parts.push("缩小");
                }
                let dir = if parts.is_empty() {
                    "云台动作".to_string()
                } else {
                    parts.join("+")
                };
                return format!(
                    "PTZ {dir}(水平{} 垂直{} 变倍{})",
                    m.pan_speed, m.tilt_speed, m.zoom_speed
                );
            }
            return "PTZ 停止".into();
        }
        // 预置位。
        if let Some((action, idx)) = ctrl.preset_op() {
            let a = match action {
                PresetAction::Set => "设置",
                PresetAction::Call => "调用",
                PresetAction::Delete => "删除",
            };
            return format!("PTZ {a}预置位 {idx}");
        }
        // 巡航。
        if let Some(op) = ctrl.cruise_op() {
            return match op {
                CruiseOp::SetPoint { track, preset } => format!("巡航{track}增点(预置{preset})"),
                CruiseOp::DelPoint { track, preset } => format!("巡航{track}删点(预置{preset})"),
                CruiseOp::DelTrack { track } => format!("删除整条巡航{track}"),
                CruiseOp::SetSpeed { track, speed } => format!("巡航{track}设速{speed}"),
                CruiseOp::SetDwell { track, dwell } => format!("巡航{track}停留{dwell}s"),
                CruiseOp::Start { track } => {
                    if track == 0 {
                        "巡航停止".into()
                    } else {
                        format!("巡航{track}启动")
                    }
                }
            };
        }
        // 自动扫描(表 A.10)。
        if let Some(op) = ctrl.scan_op() {
            use gb28181_protocol::manscdp::ScanOp;
            return match op {
                ScanOp::Start { group } => format!("自动扫描{group}开始"),
                ScanOp::SetLeftBoundary { group } => format!("自动扫描{group}设左边界"),
                ScanOp::SetRightBoundary { group } => format!("自动扫描{group}设右边界"),
                ScanOp::SetSpeed { group, speed } => format!("自动扫描{group}设速{speed}"),
            };
        }
        // 辅助控制。
        if let Some(aux) = ctrl.aux_op() {
            return format!(
                "辅助{}{}",
                aux.function.label(),
                if aux.on { "开" } else { "关" }
            );
        }
        // FI 光圈/聚焦。
        if let Some(fi) = ctrl.fi_op() {
            return fi.label();
        }
        // 其它:录像/布防/复位/看守位/精准云台/升级/抓拍/配置等,用 kind()。
        if let Some(v) = ctrl.record_cmd.as_deref() {
            return format!("录像控制:{v}");
        }
        if let Some(g) = &ctrl.guard_cmd {
            return format!("布防/撤防:{g}");
        }
        if let Some(p) = &ctrl.ptz_precise_ctrl {
            return format!("精确云台 Pan{} Tilt{} Zoom{}", p.pan, p.tilt, p.zoom);
        }
        ctrl.kind().to_string()
    }

    /// 处理设备控制命令(PTZ/关键帧/录像/布防/复位/重启),返回应答 XML。
    /// 语义:模拟器接受并回 Result=OK;强制关键帧会触发当前推流立即发关键帧
    ///(当前 FileSource/LightSource 本就周期性带关键帧,记录日志即可)。
    pub(crate) async fn handle_control(
        &self,
        ctrl: &gb28181_protocol::manscdp::Control,
    ) -> Result<String> {
        tracing::info!(kind = %ctrl.kind(), sn = ctrl.sn, "收到设备控制");
        if let Some(ptz) = &ctrl.ptz_cmd {
            // PTZ 8 字节码:识别预置位设置/调用/删除并更新预置位表;其余为方向/变倍。
            use gb28181_protocol::manscdp::PresetAction;
            match ctrl.preset_op() {
                Some((PresetAction::Set, idx)) => {
                    let name = ctrl
                        .ptz_cmd_params
                        .as_ref()
                        .and_then(|params| params.preset_name.as_deref())
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("预置位{idx}"));
                    self.presets.lock().insert(idx, name.clone());
                    tracing::info!(preset = idx, preset_name = %name, "预置位设置");
                }
                Some((PresetAction::Delete, idx)) => {
                    self.presets.lock().remove(&idx);
                    tracing::info!(preset = idx, "预置位删除");
                }
                Some((PresetAction::Call, idx)) => {
                    let name = self
                        .presets
                        .lock()
                        .get(&idx)
                        .cloned()
                        .unwrap_or_else(|| format!("预置位{idx}"));
                    tracing::info!(preset = idx, preset_name = %name, "预置位调用(转到)");
                    self.observer
                        .on_event(common::DeviceEvent::PtzPresetCall { preset: idx, name });
                }
                None => {
                    // 优先级:巡航/辅助(0x8x)→ FI 光圈聚焦(0x4x)→ 方向/变倍运动。
                    // 自动扫描(表 A.10)。0x89/0x8A 之前被错当成辅助开关(实际是
                    // 0x8C/0x8D),扫描指令等于被静默吞掉;现在单独一支。
                    if let Some(op) = ctrl.scan_op() {
                        self.apply_scan_op(op);
                    } else if let Some(op) = ctrl.cruise_op() {
                        self.apply_cruise_op(op);
                    } else if let Some(aux) = ctrl.aux_op() {
                        tracing::info!(func = aux.function.label(), on = aux.on, "辅助控制");
                    } else if let Some(fi) = ctrl.fi_op() {
                        tracing::info!(fi = %fi.label(), "FI 光圈/聚焦控制");
                    } else if let Some(m) = ctrl.ptz_motion() {
                        // 方向/变倍运动:解析后上报观察者(供 UI 云台动画)。
                        tracing::info!(ptz = %ptz, up=m.up, down=m.down, left=m.left, right=m.right,
                            zoom_in=m.zoom_in, zoom_out=m.zoom_out, "PTZ 云台运动");
                        self.observer.on_event(common::DeviceEvent::Ptz {
                            up: m.up,
                            down: m.down,
                            left: m.left,
                            right: m.right,
                            zoom_in: m.zoom_in,
                            zoom_out: m.zoom_out,
                            pan_speed: m.pan_speed,
                            tilt_speed: m.tilt_speed,
                            zoom_speed: m.zoom_speed,
                        });
                    } else {
                        tracing::info!(ptz = %ptz, "PTZ 云台控制(模拟接受)");
                    }
                }
            }
        }
        // 精确云台控制(GB-2022):记录姿态供 PTZPosition 读回。
        if let Some(p) = &ctrl.ptz_precise_ctrl {
            tracing::info!(pan = p.pan, tilt = p.tilt, zoom = p.zoom, "精确云台控制");
            self.control_state.lock().precise_pose = (p.pan, p.tilt, p.zoom);
        }
        // 目标跟踪:白名单模式外忽略,仍应答 200/OK。
        if let Some(t) = &ctrl.target_track {
            let mode = t.mode.trim();
            if matches!(mode, "Auto" | "Manual" | "Stop") {
                tracing::info!(mode, object = ?t.object_id, "目标跟踪");
            } else {
                tracing::info!(mode, "目标跟踪(模式未识别,忽略)");
            }
        }
        // 格式化 SD 卡:模拟设备无实际存储,仅记录应答。
        if ctrl.format_sd_card.is_some() {
            let disk = ctrl.disk_num.unwrap_or(0);
            tracing::info!(disk, "格式化 SD 卡(模拟接受)");
        }
        // 录像控制:更新录像态(供 DeviceStatus 的 Record 字段读回)。
        if let Some(v) = ctrl.record_cmd.as_deref() {
            let recording = v.eq_ignore_ascii_case("Record");
            self.control_state.lock().recording = recording;
            tracing::info!(record = %v, recording, "录像控制");
        }
        // 布防/撤防:更新报警值守态(供 AlarmStatus 查询读回)。
        if let Some(g) = &ctrl.guard_cmd {
            let arm = g.eq_ignore_ascii_case("SetGuard");
            self.control_state.lock().alarming = arm;
            tracing::info!(guard = %g, "布防/撤防");
        }
        // 看守位设置:更新启用/已设标志(供 HomePositionQuery 读回)。
        if let Some(hp) = &ctrl.home_position {
            {
                let mut s = self.control_state.lock();
                s.home_enabled = hp.enabled != 0;
                s.home_set = hp.enabled != 0;
            }
            tracing::info!(enabled = hp.enabled, preset = hp.preset_index, "看守位设置");
        }
        if ctrl.iframe_cmd.is_some() {
            // A.2.3.1.7:把请求交给推流循环。实时编码源(屏幕采集 H.264)会真的
            // 让编码器立刻插一个 IDR;文件循环源无法兑现,只能等自带的周期 IDR。
            let requested = match self.session.lock().await.as_ref() {
                Some(session) => {
                    session.control.request_keyframe();
                    true
                }
                None => false,
            };
            if requested {
                tracing::info!("强制关键帧:已下发到推流循环");
            } else {
                tracing::info!("强制关键帧:当前无活跃推流会话,忽略");
            }
            self.observer
                .on_event(common::DeviceEvent::PlatformCommand {
                    kind: "control".into(),
                    summary: if requested {
                        "平台请求强制关键帧:已下发到编码器".into()
                    } else {
                        "平台请求强制关键帧:当前无推流会话".to_string()
                    },
                });
        }
        // DeviceConfig 控制命令(GB28181-2022 A.2.3.2):设备配置修改。
        // 模拟器接受所有配置修改并回 OK,实际不改变设备行为(仅记录日志)。
        // CmdType=DeviceConfig 时应答须为 DeviceConfig(A.2.6.8),而非 DeviceControl。
        let is_device_config = ctrl.cmd_type == "DeviceConfig"
            || ctrl.cfg_basic_param.is_some()
            || ctrl.cfg_video_record_plan.is_some()
            || ctrl.cfg_video_alarm_record.is_some()
            || ctrl.cfg_picture_mask.is_some()
            || ctrl.cfg_frame_mirror.is_some()
            || ctrl.cfg_alarm_report.is_some()
            || ctrl.cfg_osd_config.is_some()
            || ctrl.cfg_video_param_attribute.is_some()
            || ctrl.cfg_svac_encode.is_some()
            || ctrl.cfg_svac_decode.is_some();
        if let Some(bp) = &ctrl.cfg_basic_param {
            // A.2.3.2.1:配置修改命令可只带部分字段,空串/0 表示"这一项不改"。
            let mut applied: Vec<String> = Vec::new();
            {
                let mut rt = self.runtime.lock();
                if !bp.name.trim().is_empty() && bp.name != rt.device_name {
                    applied.push(format!("Name={}", bp.name));
                    rt.device_name = bp.name.trim().to_string();
                }
                if bp.expiration > 0 && bp.expiration != rt.expiration_secs {
                    applied.push(format!("Expiration={}", bp.expiration));
                    rt.expiration_secs = bp.expiration;
                }
                if bp.heartbeat_interval > 0
                    && u64::from(bp.heartbeat_interval) != rt.heartbeat_interval_secs
                {
                    applied.push(format!("HeartBeatInterval={}", bp.heartbeat_interval));
                    rt.heartbeat_interval_secs = u64::from(bp.heartbeat_interval);
                }
                if bp.heartbeat_count > 0 && bp.heartbeat_count != rt.heartbeat_count {
                    applied.push(format!("HeartBeatCount={}", bp.heartbeat_count));
                    rt.heartbeat_count = bp.heartbeat_count;
                }
            }
            if applied.is_empty() {
                tracing::info!("设备配置:基本参数无变化");
            } else {
                tracing::info!(changed = %applied.join(", "), "设备配置:基本参数已生效");
                self.observer
                    .on_event(common::DeviceEvent::PlatformCommand {
                        kind: "control".into(),
                        summary: format!("平台改设备配置已生效:{}", applied.join(", ")),
                    });
            }
        }
        if ctrl.cfg_video_record_plan.is_some() {
            tracing::info!("设备配置:录像计划(模拟接受)");
        }
        if ctrl.cfg_video_alarm_record.is_some() {
            tracing::info!("设备配置:报警录像(模拟接受)");
        }
        if ctrl.cfg_picture_mask.is_some() {
            tracing::info!("设备配置:画面遮挡(模拟接受)");
        }
        if ctrl.cfg_frame_mirror.is_some() {
            tracing::info!("设备配置:画面翻转(模拟接受)");
        }
        if ctrl.cfg_alarm_report.is_some() {
            tracing::info!("设备配置:报警上报开关(模拟接受)");
        }
        if let Some(osd) = &ctrl.cfg_osd_config {
            let time_show = osd.time_show_flag != 0;
            let osd_show = osd.osd_show_flag != 0;
            tracing::info!(time_show, osd_show, "设备配置:前端OSD(已应用)");
            // 上报观察者:供 UI 展示"平台下发 OSD 配置、设备已应用"(国标 A.2.3.2.11)。
            self.observer.on_event(common::DeviceEvent::OsdConfig {
                time_show,
                osd_show,
            });
        }
        let resp = if is_device_config {
            gb28181_protocol::manscdp::ControlResponse::config_ok(&ctrl.device_id, ctrl.sn)
        } else {
            gb28181_protocol::manscdp::ControlResponse::ok(&ctrl.device_id, ctrl.sn)
        };
        resp.to_xml()
    }

    /// 应用自动扫描控制操作(GB/T 28181-2022 表 A.10)。
    pub(crate) fn apply_scan_op(&self, op: gb28181_protocol::manscdp::ScanOp) {
        use gb28181_protocol::manscdp::ScanOp;
        let mut s = self.control_state.lock();
        match op {
            ScanOp::Start { group } => {
                s.scan_groups.entry(group).or_default().scanning = true;
                tracing::info!(group, "自动扫描开始(画面自右向左)");
            }
            ScanOp::SetLeftBoundary { group } => {
                s.scan_groups.entry(group).or_default().left_set = true;
                tracing::info!(group, "设置自动扫描左边界");
            }
            ScanOp::SetRightBoundary { group } => {
                s.scan_groups.entry(group).or_default().right_set = true;
                tracing::info!(group, "设置自动扫描右边界");
            }
            ScanOp::SetSpeed { group, speed } => {
                s.scan_groups.entry(group).or_default().speed = speed;
                tracing::info!(group, speed, "设置自动扫描速度");
            }
        }
    }

    /// 应用巡航轨迹控制操作到设备控制态(供 CruiseTrack* 查询读回)。
    pub(crate) fn apply_cruise_op(&self, op: gb28181_protocol::manscdp::CruiseOp) {
        use gb28181_protocol::manscdp::CruiseOp;
        let mut s = self.control_state.lock();
        match op {
            CruiseOp::SetPoint { track, preset } => {
                let pts = s.cruise_tracks.entry(track).or_default();
                if !pts.contains(&preset) {
                    pts.push(preset);
                }
                tracing::info!(track, preset, "巡航增点");
            }
            CruiseOp::DelPoint { track, preset } => {
                if let Some(pts) = s.cruise_tracks.get_mut(&track) {
                    pts.retain(|&p| p != preset);
                }
                tracing::info!(track, preset, "巡航删点");
            }
            // 表 A.9 注2:字节6=00H 时删除整条巡航。
            CruiseOp::DelTrack { track } => {
                s.cruise_tracks.remove(&track);
                s.cruise_params.remove(&track);
                tracing::info!(track, "巡航删除整条轨迹");
            }
            CruiseOp::SetSpeed { track, speed } => {
                s.cruise_tracks.entry(track).or_default();
                s.cruise_params.entry(track).or_default().0 = speed;
                tracing::info!(track, speed, "巡航设速");
            }
            CruiseOp::SetDwell { track, dwell } => {
                s.cruise_tracks.entry(track).or_default();
                s.cruise_params.entry(track).or_default().1 = dwell;
                tracing::info!(track, dwell, "巡航设停留");
            }
            CruiseOp::Start { track } => {
                if track == 0 {
                    tracing::info!("巡航停止");
                } else {
                    s.cruise_tracks.entry(track).or_default();
                    tracing::info!(track, "巡航启动");
                }
            }
        }
    }

    /// 触发需设备主动 NOTIFY/上报的控制副作用(在线升级 4 步进度、抓拍上传)。
    /// 已在入站循环里回过 200/结果,这里异步执行不阻塞应答。
    pub(crate) fn spawn_control_side_effects(
        self: &Arc<Self>,
        transport: &Arc<dyn SipTransport>,
        ctrl: &gb28181_protocol::manscdp::Control,
    ) {
        // 在线升级:4 步进度 NOTIFY。
        if let Some(up) = &ctrl.device_upgrade {
            let dev = Arc::clone(self);
            let tp = Arc::clone(transport);
            let up = up.clone();
            tokio::spawn(async move {
                dev.run_upgrade_progress(&tp, &up).await;
            });
        }
        // 抓拍配置(GB-2022):HTTP 上传 + 完成 NOTIFY。
        if let Some(cfg) = &ctrl.snap_shot_config {
            if cfg.is_valid() {
                let dev = Arc::clone(self);
                let tp = Arc::clone(transport);
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    dev.run_snapshot_upload(&tp, &cfg).await;
                });
            } else {
                tracing::warn!("SnapShotConfig 缺 SessionID/UploadURL,忽略");
            }
        }
        // 抓拍(旧路径):经 Alarm Notify 上报。
        if ctrl.snap_shot_cmd.is_some() {
            let dev = Arc::clone(self);
            let tp = Arc::clone(transport);
            tokio::spawn(async move {
                let (h, p) = (dev.local_host(), dev.local_port());
                let _ = dev.report_alarm(&tp, &h, p, "SnapShot").await;
            });
        }
    }

    /// 在线升级 4 步进度:percent [0,30,60,100],步间 1.5s,发 DeviceUpgradeResult NOTIFY。
    pub(crate) async fn run_upgrade_progress(
        &self,
        transport: &Arc<dyn SipTransport>,
        up: &gb28181_protocol::manscdp::DeviceUpgrade,
    ) {
        let (h, p) = (self.local_host(), self.local_port());
        for percent in [0u8, 30, 60, 100] {
            let sn = self.next_cseq() & 0xFFFF;
            let notify = gb28181_protocol::manscdp::DeviceUpgradeResultNotify::new(
                self.config.device_id.as_str(),
                sn,
                &up.session_id,
                &up.firmware,
                percent,
            );
            if let Ok(xml) = notify.to_xml() {
                let _ = self.send_message_xml(transport, &h, p, &xml).await;
            }
            tracing::info!(percent, "在线升级进度");
            let step = match percent {
                0 => 1,
                30 => 2,
                60 => 3,
                _ => 4,
            };
            self.observer.on_event(common::DeviceEvent::Progress {
                kind: "upgrade".into(),
                current: step,
                total: 4,
                percent: percent as u32,
            });
            if percent < 100 {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
        }
    }
}
