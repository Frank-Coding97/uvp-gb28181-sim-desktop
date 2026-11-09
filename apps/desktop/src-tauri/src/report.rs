//! 压测报告渲染(FR-28)。
//!
//! 输出一份**自包含单文件 HTML**:样式与图表全部内联,不引用任何外部资源,
//! 双击即可用浏览器打开,也可以直接「打印 → 存为 PDF」交付给客户。
//!
//! 约束:
//! - 只做渲染:输入 [`RunRecord`](已脱敏的场景 + 秒级指标采样),除写文件外无副作用。
//! - 所有注入文本都过 [`esc`]:设备名、平台地址来自配置文件,可能含 `<` 或 `&`。
//! - 图表用纯 SVG / CSS 绘制,不引 ECharts 之类 CDN —— 内网与离线环境同样能看。

use std::fmt::Write as _;

use stress_engine::MetricsSnapshot;

/// 秒级指标采样点(趋势图数据)。
#[derive(Debug, Clone)]
pub struct Sample {
    /// 相对压测启动的秒数。
    pub offset_secs: u64,
    pub snapshot: MetricsSnapshot,
}

/// 单次压测的完整记录 —— 报告的唯一数据源。
#[derive(Debug, Clone)]
pub struct RunRecord {
    /// 启动 / 结束时间(本地 UNIX 毫秒)。运行中导出时 `finished_at_ms` 为 0。
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub device_count: usize,
    /// 目标平台描述,如 `192.168.1.10:5060 · TCP`。
    pub target: String,
    /// 国标版本,如 `GB/T 28181-2022`。
    pub gb_version: String,
    /// 场景参数(中文标签 → 已脱敏的值)。
    pub scenario_rows: Vec<(String, String)>,
    /// 脱敏后的场景 TOML 原文,作为可复现凭据。
    pub scenario_toml_redacted: String,
    pub samples: Vec<Sample>,
    pub metrics: MetricsSnapshot,
}

/// 采样上限:1 秒 1 点,留够 2 小时;超出后抽稀,避免长时间压测把内存吃穿。
pub const MAX_SAMPLES: usize = 7_200;

/// 趋势图最多画多少个点(点更多也看不出差别,反而把 SVG 撑大)。
const CHART_MAX_POINTS: usize = 300;

// ── 对外入口 ───────────────────────────────────────────────

/// 渲染完整报告 HTML。
pub fn render_html(record: &RunRecord) -> String {
    let m = &record.metrics;
    let attempted = m.register_attempted;
    let failures = m.register_failed;
    let heat = m.fail_timeout + m.fail_rejected + m.fail_other;
    let heartbeat_total = m.heartbeat_succeeded + m.heartbeat_failed;

    let (label, tone, advice) = verdict(m.register_success_rate, attempted);
    let duration_ms = if record.finished_at_ms > record.started_at_ms {
        record.finished_at_ms - record.started_at_ms
    } else {
        common::clock::local_epoch_ms() - record.started_at_ms
    };
    let finished_at = if record.finished_at_ms > 0 {
        record.finished_at_ms
    } else {
        common::clock::local_epoch_ms()
    };
    let running = record.finished_at_ms == 0;

    let mut h = String::with_capacity(48 * 1024);
    h.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(
        h,
        "<title>压测报告 · {} · {}</title>",
        esc(&record.target),
        esc(&iso_local_hint(record.started_at_ms))
    );
    h.push_str("<style>\n");
    h.push_str(CSS);
    h.push_str("</style>\n</head>\n<body>\n<div class=\"page\">\n");

    // 页眉
    h.push_str("<header class=\"hd\">\n<div>\n<div class=\"brand\">UVP GB28181 桌面端 · 压力测试</div>\n");
    h.push_str("<h1>设备接入压力测试报告</h1>\n<div class=\"meta\">");
    let _ = write!(
        h,
        "目标平台 <b>{}</b> · {} · 模拟 {} 台设备",
        esc(&record.target),
        esc(&record.gb_version),
        fmt_int(record.device_count as u64)
    );
    h.push_str("</div>\n</div>\n<div class=\"stamp\">");
    let _ = write!(
        h,
        "<b>{}</b>开始 {}<br>结束 {}<br>时长 {}",
        if running {
            "<span class=\"live\">● 运行中快照</span><br>"
        } else {
            ""
        },
        time_tag(record.started_at_ms),
        if running {
            "—".to_string()
        } else {
            time_tag(finished_at)
        },
        fmt_duration(duration_ms)
    );
    h.push_str("</div>\n</header>\n");

    // 结论
    let _ = write!(
        h,
        "<section class=\"verdict tone-{tone}\">\n<div class=\"score\">{:.2}<span>%</span></div>\n<div class=\"body\">\n<h2>注册成功率 · {label}</h2>\n<p>{advice}</p>\n<p class=\"facts\">{facts}</p>\n</div>\n</section>\n",
        m.register_success_rate * 100.0,
        tone = tone,
        label = label,
        advice = esc(advice),
        facts = esc(&summary_sentence(record, duration_ms, heat)),
    );

    // KPI 卡片
    let peak_streams = record
        .samples
        .iter()
        .map(|s| s.snapshot.active_streams)
        .max()
        .unwrap_or(m.active_streams);
    h.push_str("<section class=\"kpis\">\n");
    kpi(&mut h, "模拟设备数", &fmt_int(record.device_count as u64), "台虚拟国标设备");
    kpi(
        &mut h,
        "注册成功 / 尝试",
        &format!(
            "{} / {}",
            fmt_int(m.register_succeeded),
            fmt_int(attempted)
        ),
        &format!("成功率 {}", pct(m.register_success_rate, 2)),
    );
    kpi(
        &mut h,
        "注册失败",
        &fmt_int(failures),
        &format!(
            "超时 {} · 拒绝 {} · 其它 {}",
            fmt_int(m.fail_timeout),
            fmt_int(m.fail_rejected),
            fmt_int(m.fail_other)
        ),
    );
    kpi(
        &mut h,
        "心跳成功率",
        &pct(ratio(m.heartbeat_succeeded, heartbeat_total), 1),
        &format!(
            "成功 {} · 失败 {}",
            fmt_int(m.heartbeat_succeeded),
            fmt_int(m.heartbeat_failed)
        ),
    );
    kpi(
        &mut h,
        "活跃推流",
        &format!("{} 路", fmt_int(peak_streams)),
        &format!("峰值 · 结束时 {}", fmt_int(m.active_streams)),
    );
    kpi(
        &mut h,
        "运行时长",
        &fmt_duration(duration_ms),
        &format!(
            "主动上报 定位 {} · 报警 {}",
            fmt_int(m.position_reported),
            fmt_int(m.alarm_reported)
        ),
    );
    h.push_str("</section>\n");

    // 趋势图
    let rate_series = series_from(record, |s| s.register_success_rate * 100.0);
    let stream_series = series_from(record, |s| s.active_streams as f64);
    h.push_str("<section class=\"row two\">\n<div class=\"card\">\n<h3>注册成功率趋势<small>每秒采样</small></h3>\n");
    if rate_series.len() >= 2 {
        let min = rate_series
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::INFINITY, f64::min);
        let floor = if min >= 90.0 {
            ((min * 10.0).floor() / 10.0 - 0.2).max(90.0)
        } else {
            0.0
        };
        let ticks = tick_values(floor, 100.0, 4);
        h.push_str(&line_chart(
            &rate_series,
            floor,
            100.0,
            &ticks,
            Some((90.0, "90% 参考线")),
            |v| format!("{v:.1}%"),
            "#2f6fed",
        ));
    } else {
        h.push_str(&empty_chart("采样点不足,未生成趋势图"));
    }
    h.push_str("</div>\n<div class=\"card\">\n<h3>活跃推流趋势<small>路</small></h3>\n");
    if stream_series.len() >= 2 {
        let max = stream_series.iter().map(|(_, v)| *v).fold(0.0, f64::max);
        let top = nice_ceil(max.max(1.0));
        let ticks = tick_values(0.0, top, 4);
        h.push_str(&line_chart(
            &stream_series,
            0.0,
            top,
            &ticks,
            None,
            |v| fmt_int(v.round() as u64),
            "#0f9d76",
        ));
    } else {
        h.push_str(&empty_chart("本次压测没有推流设备"));
    }
    h.push_str("</div>\n</section>\n");

    // 失败归因 + 心跳
    h.push_str("<section class=\"row two-even\">\n<div class=\"card\">\n<h3>注册失败归因<small>共 ");
    let _ = write!(h, "{} 次</small></h3>\n", fmt_int(failures));
    if failures == 0 {
        h.push_str("<div class=\"allgood\"><span>✓</span><div><b>本次没有注册失败</b><p>平台对所有设备的注册请求都给出了成功响应。</p></div></div>\n");
    } else {
        // 归因三项之和理论上等于失败总数;若不等(例如旧版本引擎只记总数),
        // 差额单列「未归因」,避免环形图算出来的比例和标题对不上。
        let unclassified = failures.saturating_sub(heat);
        h.push_str("<div class=\"donut-wrap\">\n");
        h.push_str(&donut_svg(&[
            (m.fail_timeout as f64, "#f0a23b"),
            (m.fail_rejected as f64, "#e05545"),
            (m.fail_other as f64, "#8b93a7"),
            (unclassified as f64, "#c4c9d4"),
        ]));
        h.push_str("<div class=\"legend\">\n");
        legend_row(&mut h, "#f0a23b", "超时无响应", m.fail_timeout, failures);
        legend_row(
            &mut h,
            "#e05545",
            "平台拒绝(4xx/5xx)",
            m.fail_rejected,
            failures,
        );
        legend_row(&mut h, "#8b93a7", "网络 / 其它", m.fail_other, failures);
        if unclassified > 0 {
            legend_row(&mut h, "#c4c9d4", "未归因", unclassified, failures);
        }
        h.push_str("</div>\n</div>\n");
    }
    h.push_str("</div>\n<div class=\"card\">\n<h3>心跳与主动上报<small>累计次数</small></h3>\n");

    h.push_str("<div class=\"stack\">\n<div class=\"stack-head\"><span>心跳</span><b>");
    let _ = write!(h, "{} 成功 / {} 失败", fmt_int(m.heartbeat_succeeded), fmt_int(m.heartbeat_failed));
    h.push_str("</b></div>\n<div class=\"bar\">");
    let ok_w = if heartbeat_total == 0 {
        0.0
    } else {
        m.heartbeat_succeeded as f64 / heartbeat_total as f64 * 100.0
    };
    let _ = write!(
        h,
        "<i class=\"ok\" style=\"width:{ok_w:.2}%\"></i><i class=\"bad\" style=\"width:{:.2}%\"></i>",
        100.0 - ok_w
    );
    h.push_str("</div>\n</div>\n");

    let report_total = m.position_reported + m.alarm_reported;
    h.push_str("<div class=\"stack\">\n<div class=\"stack-head\"><span>主动上报</span><b>");
    let _ = write!(h, "{} 条", fmt_int(report_total));
    h.push_str("</b></div>\n<div class=\"mini-row\">");
    let _ = write!(
        h,
        "<span>定位 <b>{}</b></span><span>报警 <b>{}</b></span>",
        fmt_int(m.position_reported),
        fmt_int(m.alarm_reported)
    );
    h.push_str("</div>\n</div>\n");
    h.push_str("<p class=\"note\">心跳按设备心跳周期累计;主动上报仅在压测时配置了周期才会产生。</p>\n");
    h.push_str("</div>\n</section>\n");

    // 指标明细
    h.push_str("<section class=\"card\">\n<h3>指标明细<small>压测结束时的累计值</small></h3>\n");
    h.push_str("<table>\n<thead><tr><th style=\"width:38%\">指标</th><th class=\"num\" style=\"width:18%\">数值</th><th>说明</th></tr></thead>\n<tbody>\n");
    metric_row(&mut h, "注册尝试", &fmt_int(attempted), "设备发出的 REGISTER 次数");
    metric_row(
        &mut h,
        "注册成功",
        &fmt_int(m.register_succeeded),
        "收到平台 200 OK 的次数",
    );
    metric_row(&mut h, "注册失败", &fmt_int(failures), "未成功的注册次数");
    metric_row(
        &mut h,
        "注册成功率",
        &pct(m.register_success_rate, 2),
        "注册成功 ÷ 注册尝试",
    );
    metric_row(
        &mut h,
        "失败 · 超时",
        &fmt_int(m.fail_timeout),
        "等待平台响应超时",
    );
    metric_row(
        &mut h,
        "失败 · 平台拒绝",
        &fmt_int(m.fail_rejected),
        "平台返回 4xx / 5xx",
    );
    metric_row(
        &mut h,
        "失败 · 网络或其它",
        &fmt_int(m.fail_other),
        "网络不可达、解析失败等",
    );
    metric_row(
        &mut h,
        "心跳成功",
        &fmt_int(m.heartbeat_succeeded),
        "MESSAGE 心跳收到成功响应",
    );
    metric_row(
        &mut h,
        "心跳失败",
        &fmt_int(m.heartbeat_failed),
        "心跳超时或响应异常",
    );
    metric_row(
        &mut h,
        "活跃推流",
        &format!("{} 路", fmt_int(m.active_streams)),
        "结束时仍在推流的设备数",
    );
    metric_row(
        &mut h,
        "定位上报",
        &fmt_int(m.position_reported),
        "主动定位上报成功次数",
    );
    metric_row(
        &mut h,
        "报警上报",
        &fmt_int(m.alarm_reported),
        "主动报警上报成功次数",
    );
    h.push_str("</tbody>\n</table>\n</section>\n");

    // 场景参数
    if !record.scenario_rows.is_empty() {
        h.push_str("<section class=\"card\">\n<h3>压测参数<small>本次运行使用的场景配置</small></h3>\n<table>\n<tbody>\n");
        for (label, value) in &record.scenario_rows {
            let _ = writeln!(
                h,
                "<tr><td class=\"k\">{}</td><td>{}</td></tr>",
                esc(label),
                esc(value)
            );
        }
        h.push_str("</tbody>\n</table>\n");
        if !record.scenario_toml_redacted.trim().is_empty() {
            h.push_str("<details class=\"toml\">\n<summary>查看可复现的场景参数(TOML,密码已隐藏)</summary>\n<pre>");
            h.push_str(&esc(&record.scenario_toml_redacted));
            h.push_str("</pre>\n</details>\n");
        }
        h.push_str("</section>\n");
    }

    // 页脚
    h.push_str("<footer>\n<p><b>判定口径</b>　注册成功率 ≥ 99.9% 为优秀,≥ 99% 为良好,≥ 95% 为需关注,低于 95% 为不达标。该阈值用于快速判断平台在本轮并发下的接入稳定性,不替代业务侧验收标准。</p>\n");
    h.push_str("<p><b>数据说明</b>　指标由压测引擎在设备注册、心跳、推流路径上实时累计;趋势图为每秒采样,长时间压测会按比例抽稀。报告不含注册密码,场景参数中的密码已替换为 ***。</p>\n");
    h.push_str("<p class=\"gen\">本报告由 UVP GB28181 桌面端自动生成 · 可导出为 PDF 后归档</p>\n</footer>\n</div>\n");

    h.push_str(TIME_SCRIPT);
    h.push_str("</body>\n</html>\n");
    h
}

/// 渲染机器可读的 JSON 附属文件(与 HTML 同目录、同文件名)。
pub fn render_json(record: &RunRecord) -> String {
    let m = &record.metrics;
    let finished = if record.finished_at_ms > 0 {
        record.finished_at_ms
    } else {
        common::clock::local_epoch_ms()
    };
    let value = serde_json::json!({
        "generated_at_epoch_secs": finished / 1000,
        "started_at_epoch_secs": record.started_at_ms / 1000,
        "finished_at_epoch_secs": finished / 1000,
        "duration_secs": ((finished - record.started_at_ms).max(0)) / 1000,
        "target": record.target,
        "gb_version": record.gb_version,
        "device_count": record.device_count,
        "scenario_toml_redacted": record.scenario_toml_redacted,
        "metrics": m,
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
}

// ── 结论与文案 ─────────────────────────────────────────────

/// 成功率分档 → (评级, 样式 tone, 建议)。抽成纯函数便于单测。
fn verdict(rate: f64, attempted: u64) -> (&'static str, &'static str, &'static str) {
    if attempted == 0 {
        return (
            "数据不足",
            "warn",
            "本次压测没有采集到注册尝试,请确认设备是否成功启动。",
        );
    }
    if rate >= 0.999 {
        (
            "优秀",
            "good",
            "平台在本次压力下注册几乎无失败,接入链路稳定。",
        )
    } else if rate >= 0.99 {
        (
            "良好",
            "good",
            "整体稳定,仅个别设备注册失败,可结合失败归因继续观察。",
        )
    } else if rate >= 0.95 {
        (
            "需关注",
            "warn",
            "已有部分设备注册失败,平台或链路开始出现压力,建议降低并发或排查失败原因。",
        )
    } else {
        (
            "不达标",
            "bad",
            "注册失败比例偏高,当前并发已超出平台承载能力,建议先排查平台容量与网络链路。",
        )
    }
}

/// 一句话结论:把关键数字读出来,让人不用看表也知道发生了什么。
fn summary_sentence(record: &RunRecord, duration_ms: i64, heat: u64) -> String {
    let m = &record.metrics;
    let mut s = format!(
        "本次模拟 {} 台设备接入 {}，持续 {}，注册成功率 {}。",
        fmt_int(record.device_count as u64),
        record.target,
        fmt_duration(duration_ms),
        pct(m.register_success_rate, 2),
    );
    if m.register_failed > 0 {
        let _ = write!(
            s,
            "共 {} 次注册失败，其中超时 {} 次、平台拒绝 {} 次、网络或其它 {} 次。",
            fmt_int(m.register_failed),
            fmt_int(m.fail_timeout),
            fmt_int(m.fail_rejected),
            fmt_int(m.fail_other),
        );
        if heat == 0 {
            s.push_str("失败未带归因明细，建议一并检查平台日志。");
        }
    } else {
        s.push_str("没有出现注册失败。");
    }
    if m.heartbeat_failed > 0 {
        let _ = write!(s, "另有 {} 次心跳失败。", fmt_int(m.heartbeat_failed));
    }
    s
}

// ── 片段渲染 ───────────────────────────────────────────────

fn kpi(h: &mut String, label: &str, value: &str, note: &str) {
    let _ = writeln!(
        h,
        "<div class=\"kpi\"><div class=\"k\">{}</div><div class=\"v\">{}</div><div class=\"n\">{}</div></div>",
        esc(label),
        esc(value),
        esc(note)
    );
}

fn metric_row(h: &mut String, name: &str, value: &str, note: &str) {
    let _ = writeln!(
        h,
        "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"note\">{}</td></tr>",
        esc(name),
        esc(value),
        esc(note)
    );
}

fn legend_row(h: &mut String, color: &str, label: &str, value: u64, total: u64) {
    let share = ratio(value, total) * 100.0;
    let _ = writeln!(
        h,
        "<span><i class=\"swatch\" style=\"background:{color}\"></i>{}<b>{} · {:.0}%</b></span>",
        esc(label),
        fmt_int(value),
        share
    );
}

fn empty_chart(text: &str) -> String {
    format!(
        "<div class=\"empty\">{}</div>",
        esc(text)
    )
}

/// 从采样里抽某一维,并按上限抽稀。
fn series_from(record: &RunRecord, pick: impl Fn(&MetricsSnapshot) -> f64) -> Vec<(u64, f64)> {
    let raw: Vec<(u64, f64)> = record
        .samples
        .iter()
        .map(|s| (s.offset_secs, pick(&s.snapshot)))
        .collect();
    decimate(raw, CHART_MAX_POINTS)
}

fn decimate(points: Vec<(u64, f64)>, max: usize) -> Vec<(u64, f64)> {
    if points.len() <= max {
        return points;
    }
    let stride = (points.len() as f64 / max as f64).ceil() as usize;
    let last = points.last().copied();
    let mut out: Vec<(u64, f64)> = points.iter().step_by(stride.max(1)).copied().collect();
    // 末点必须保留:它代表当前状态,抽稀丢掉会误导。
    if let (Some(last), Some(tail)) = (last, out.last().copied()) {
        if tail.0 != last.0 {
            out.push(last);
        }
    }
    out
}

/// 生成一条折线图(SVG)。`series` 为 (秒, 值),`ticks` 为 Y 轴刻度。
fn line_chart(
    series: &[(u64, f64)],
    y_min: f64,
    y_max: f64,
    ticks: &[f64],
    ref_line: Option<(f64, &str)>,
    value_fmt: impl Fn(f64) -> String,
    color: &str,
) -> String {
    const W: f64 = 640.0;
    const H: f64 = 196.0;
    const PL: f64 = 52.0;
    const PR: f64 = 16.0;
    const PT: f64 = 12.0;
    const PB: f64 = 26.0;
    let pw = W - PL - PR;
    let ph = H - PT - PB;
    let t_max = series.last().map(|p| p.0).unwrap_or(1).max(1) as f64;
    let span = (y_max - y_min).abs().max(f64::EPSILON);

    let px = |t: u64| PL + (t as f64 / t_max) * pw;
    let py = |v: f64| PT + (1.0 - ((v - y_min) / span).clamp(0.0, 1.0)) * ph;

    let mut s = String::with_capacity(4 * 1024);
    let _ = write!(
        s,
        "<svg class=\"chart\" viewBox=\"0 0 {w} {h}\" role=\"img\" preserveAspectRatio=\"none\">",
        w = W,
        h = H
    );

    // 背景与网格
    let _ = write!(
        s,
        "<rect x=\"{left}\" y=\"{top}\" width=\"{pw}\" height=\"{ph}\" fill=\"#fbfcfe\" stroke=\"#eef0f4\"/>",
        left = PL,
        top = PT
    );
    for tick in ticks {
        let y = py(*tick);
        let _ = write!(
            s,
            "<line x1=\"{left}\" y1=\"{y:.1}\" x2=\"{right:.1}\" y2=\"{y:.1}\" stroke=\"#eceff4\" stroke-width=\"1\"/>",
            left = PL,
            right = PL + pw
        );
        let _ = write!(
            s,
            "<text x=\"{x:.1}\" y=\"{y:.1}\" class=\"axis\" text-anchor=\"end\">{label}</text>",
            x = PL - 8.0,
            y = y + 3.5,
            label = esc(&value_fmt(*tick))
        );
    }

    // 参考线
    if let Some((value, label)) = ref_line {
        if value > y_min && value < y_max {
            let y = py(value);
            let _ = write!(
                s,
                "<line x1=\"{left}\" y1=\"{y:.1}\" x2=\"{right:.1}\" y2=\"{y:.1}\" stroke=\"#c9a227\" stroke-width=\"1\" stroke-dasharray=\"4 4\"/>",
                left = PL,
                right = PL + pw
            );
            let _ = write!(
                s,
                "<text x=\"{x:.1}\" y=\"{y:.1}\" class=\"axis\" text-anchor=\"end\" fill=\"#9a7b12\">{label}</text>",
                x = PL + pw - 4.0,
                y = y - 5.0,
                label = esc(label)
            );
        }
    }

    // 面积 + 折线
    let mut points = String::with_capacity(series.len() * 14);
    for (t, v) in series {
        let _ = write!(points, "{:.1},{:.1} ", px(*t), py(*v));
    }
    let last_x = px(series.last().map(|p| p.0).unwrap_or(0));
    let base_y = PT + ph;
    let _ = write!(
        s,
        "<polygon points=\"{left},{base_y} {pts}{last_x},{base_y}\" fill=\"{color}\" fill-opacity=\"0.10\"/>",
        left = PL,
        base_y = base_y,
        pts = points,
        last_x = last_x,
        color = color
    );
    let _ = write!(
        s,
        "<polyline points=\"{points}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"2\" stroke-linejoin=\"round\" stroke-linecap=\"round\"/>"
    );
    let last_y = py(series.last().map(|p| p.1).unwrap_or(0.0));
    let _ = write!(
        s,
        "<circle cx=\"{last_x:.1}\" cy=\"{last_y:.1}\" r=\"3.2\" fill=\"{color}\"/>"
    );

    // X 轴时间标签:起点 / 中点 / 终点
    let mid = series[series.len() / 2].0;
    for (t, anchor) in [
        (0u64, "start"),
        (mid, "middle"),
        (series.last().map(|p| p.0).unwrap_or(0), "end"),
    ] {
        let _ = write!(
            s,
            "<text x=\"{x:.1}\" y=\"{y:.1}\" class=\"axis\" text-anchor=\"{anchor}\">{label}</text>",
            x = px(t),
            y = H - 8.0,
            anchor = anchor,
            label = fmt_offset(t)
        );
    }
    let _ = write!(s, "</svg>");
    s
}

fn donut_svg(parts: &[(f64, &str)]) -> String {
    const CX: f64 = 78.0;
    const CY: f64 = 78.0;
    const R: f64 = 56.0;
    const SW: f64 = 24.0;
    let circ = 2.0 * std::f64::consts::PI * R;
    let total: f64 = parts.iter().map(|(v, _)| *v).sum();
    let mut s = String::with_capacity(900);
    let _ = write!(
        s,
        "<svg class=\"donut\" viewBox=\"0 0 156 156\" role=\"img\"><circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\" fill=\"none\" stroke=\"#eef0f4\" stroke-width=\"{sw}\"/>",
        cx = CX,
        cy = CY,
        r = R,
        sw = SW
    );
    let mut offset = 0.0;
    for (value, color) in parts {
        if *value <= 0.0 || total <= 0.0 {
            continue;
        }
        let len = value / total * circ;
        let _ = write!(
            s,
            "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"{sw}\" stroke-dasharray=\"{len:.2} {rest:.2}\" stroke-dashoffset=\"{off:.2}\" transform=\"rotate(-90 {cx} {cy})\"/>",
            cx = CX,
            cy = CY,
            r = R,
            sw = SW,
            color = color,
            len = len,
            rest = circ - len,
            off = -offset
        );
        offset += len;
    }
    let _ = write!(
        s,
        "<text x=\"{cx}\" y=\"{num_y:.1}\" class=\"donut-num\" text-anchor=\"middle\">{num}</text><text x=\"{cx}\" y=\"{sub_y:.1}\" class=\"donut-sub\" text-anchor=\"middle\">次失败</text>",
        cx = CX,
        num_y = CY - 2.0,
        num = fmt_int(total.round() as u64),
        sub_y = CY + 16.0
    );
    let _ = write!(s, "</svg>");
    s
}

// ── 数值与文本 ─────────────────────────────────────────────

fn ratio(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

fn pct(rate: f64, digits: usize) -> String {
    format!("{:.*}%", digits, rate * 100.0)
}

/// 千分位整数,`10000` → `10,000`。
fn fmt_int(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn fmt_duration(ms: i64) -> String {
    let secs = (ms.max(0) / 1000) as u64;
    if secs >= 3600 {
        format!("{} 小时 {} 分", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{} 分 {} 秒", secs / 60, secs % 60)
    } else {
        format!("{secs} 秒")
    }
}

/// 趋势图 X 轴:`85` → `1:25`。
fn fmt_offset(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// 生成 `<time>` 标签:先写 UTC 文案兜底,页面打开后由内联脚本改成本地时间。
fn time_tag(ms: i64) -> String {
    format!(
        "<time data-epoch-ms=\"{}\">{}</time>",
        ms,
        esc(&iso_local_hint(ms))
    )
}

fn iso_local_hint(ms: i64) -> String {
    format!("{} UTC", common::clock::epoch_to_iso8601(ms / 1000))
}

fn tick_values(min: f64, max: f64, count: usize) -> Vec<f64> {
    let count = count.max(2);
    let step = (max - min) / (count - 1) as f64;
    (0..count).map(|i| min + step * i as f64).collect()
}

fn nice_ceil(v: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        return 1.0;
    }
    let exp = v.log10().floor();
    let base = 10f64.powf(exp);
    // 候选取 1/2/2.5/3/4/5/10:带 3、4 是为了让 300 路这种常见峰值落在整刻度上,
    // 否则 300 会被抬到 500,曲线被压在下半区不好看。
    for m in [1.0, 2.0, 2.5, 3.0, 4.0, 5.0, 10.0] {
        if v <= m * base {
            return m * base;
        }
    }
    10.0 * base
}

/// HTML 转义。所有注入到报告里的外部字符串都必须经过这里。
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ── 静态资源 ───────────────────────────────────────────────

const CSS: &str = r##"
*{box-sizing:border-box}
body{margin:0;background:#f4f6fa;color:#101828;font-family:-apple-system,BlinkMacSystemFont,"PingFang SC","Microsoft YaHei","Segoe UI",Roboto,"Helvetica Neue",sans-serif;font-size:14px;line-height:1.6;-webkit-font-smoothing:antialiased}
.page{max-width:980px;margin:0 auto;padding:36px 30px 60px}
.hd{display:flex;justify-content:space-between;align-items:flex-start;gap:28px;padding-bottom:18px;border-bottom:2px solid #101828}
.brand{font-size:11.5px;letter-spacing:.16em;color:#667085;text-transform:uppercase}
h1{font-size:27px;line-height:1.25;margin:8px 0 10px;font-weight:700;letter-spacing:-.01em}
.meta{font-size:13px;color:#475467}
.meta b{color:#101828;font-weight:600}
.stamp{text-align:right;font-size:12px;line-height:1.9;color:#667085;white-space:nowrap}
.stamp b{color:#101828;font-weight:600}
.live{color:#b54708;font-size:11.5px}
.verdict{display:flex;align-items:center;gap:26px;margin:22px 0 18px;padding:20px 24px;background:#fff;border:1px solid #e4e7ec;border-left:5px solid #98a2b3;border-radius:12px}
.verdict .score{font-size:42px;font-weight:700;line-height:1;letter-spacing:-.02em;white-space:nowrap}
.verdict .score span{font-size:17px;font-weight:600;margin-left:2px}
.verdict h2{margin:0 0 3px;font-size:16px;font-weight:600}
.verdict p{margin:0;color:#475467;font-size:13px}
.verdict p.facts{margin-top:6px;font-size:12.5px;color:#667085}
.tone-good{border-left-color:#12805c}.tone-good .score,.tone-good h2{color:#12805c}
.tone-warn{border-left-color:#d97706}.tone-warn .score,.tone-warn h2{color:#b54708}
.tone-bad{border-left-color:#b42318}.tone-bad .score,.tone-bad h2{color:#b42318}
.kpis{display:grid;grid-template-columns:repeat(3,1fr);gap:12px;margin:0 0 14px}
.kpi{background:#fff;border:1px solid #e4e7ec;border-radius:12px;padding:14px 16px}
.kpi .k{font-size:12px;color:#667085}
.kpi .v{font-size:22px;font-weight:700;margin-top:3px;letter-spacing:-.01em}
.kpi .n{font-size:11.5px;color:#98a2b3;margin-top:3px}
.row{display:grid;gap:12px;margin:0 0 14px}
.row.two{grid-template-columns:1.32fr 1fr}
.row.two-even{grid-template-columns:1fr 1fr}
.card{background:#fff;border:1px solid #e4e7ec;border-radius:12px;padding:16px 18px}
.card h3{margin:0 0 12px;font-size:14px;font-weight:600}
.card h3 small{font-weight:400;color:#98a2b3;font-size:11.5px;margin-left:7px}
.chart{display:block;width:100%;height:196px}
.axis{font-size:10.5px;fill:#98a2b3;font-family:inherit}
.empty{height:196px;display:flex;align-items:center;justify-content:center;color:#98a2b3;font-size:12.5px;background:#fbfcfe;border:1px dashed #e4e7ec;border-radius:8px}
.donut-wrap{display:flex;align-items:center;gap:20px}
.donut{width:156px;height:156px;flex:none}
.donut-num{font-size:19px;font-weight:700;fill:#101828;font-family:inherit}
.donut-sub{font-size:10.5px;fill:#98a2b3;font-family:inherit}
.legend{display:flex;flex-direction:column;gap:9px;font-size:12.5px;flex:1}
.legend span{display:flex;align-items:center;gap:8px;color:#475467}
.legend b{margin-left:auto;color:#101828;font-variant-numeric:tabular-nums;white-space:nowrap}
.swatch{width:9px;height:9px;border-radius:3px;flex:none}
.allgood{display:flex;align-items:center;gap:14px;padding:18px 4px}
.allgood span{width:40px;height:40px;border-radius:50%;background:#e8f6f0;color:#12805c;display:flex;align-items:center;justify-content:center;font-size:20px;flex:none}
.allgood b{display:block;font-size:14px}
.allgood p{margin:2px 0 0;color:#667085;font-size:12.5px}
.stack{margin-bottom:16px}
.stack-head{display:flex;justify-content:space-between;font-size:12.5px;color:#475467;margin-bottom:6px}
.stack-head b{color:#101828;font-variant-numeric:tabular-nums;font-weight:600}
.bar{height:11px;border-radius:6px;background:#eef0f4;overflow:hidden;display:flex}
.bar i{display:block;height:100%}
.bar i.ok{background:#12805c}
.bar i.bad{background:#e05545}
.mini-row{display:flex;gap:10px}
.mini-row span{flex:1;background:#f7f8fb;border:1px solid #eef0f4;border-radius:9px;padding:9px 11px;font-size:12px;color:#667085}
.mini-row b{display:block;color:#101828;font-size:15px;font-variant-numeric:tabular-nums}
.note{color:#98a2b3;font-size:11.5px;margin:0}
table{width:100%;border-collapse:collapse;font-size:13px;table-layout:fixed}
th,td{text-align:left;padding:7px 8px;border-bottom:1px solid #f0f2f6;vertical-align:top}
th{color:#667085;font-weight:500;font-size:11.5px;letter-spacing:.02em}
td.num{text-align:right;font-variant-numeric:tabular-nums;font-weight:600;white-space:nowrap}
td.note{color:#667085;font-size:12.5px}
td.k{color:#667085;width:38%}
tbody tr:last-child td{border-bottom:none}
details.toml{margin-top:12px;border-top:1px solid #f0f2f6;padding-top:10px}
details.toml summary{cursor:pointer;color:#2f6fed;font-size:12.5px}
details.toml pre{background:#f7f8fb;border:1px solid #eef0f4;border-radius:8px;padding:12px;overflow:auto;font-size:11.5px;line-height:1.55;color:#344054;max-height:260px}
footer{margin-top:24px;padding-top:16px;border-top:1px solid #e4e7ec;color:#667085;font-size:12px}
footer p{margin:6px 0}
footer b{color:#344054}
footer .gen{color:#98a2b3;margin-top:10px}
@media print{
  @page{size:A4;margin:12mm}
  body{background:#fff;font-size:12px}
  .page{max-width:none;padding:0}
  .card,.kpi,.verdict{break-inside:avoid;page-break-inside:avoid;border-color:#dcdfe6}
  .row{break-inside:avoid}
  *{-webkit-print-color-adjust:exact;print-color-adjust:exact}
}
"##;

/// 把 UTC 兜底文案改成本地时间。量小且只读 DOM,不依赖任何库。
const TIME_SCRIPT: &str = r##"
<script>
(function(){
  function pad(n){return String(n).padStart(2,'0');}
  document.querySelectorAll('time[data-epoch-ms]').forEach(function(el){
    var ms=Number(el.getAttribute('data-epoch-ms'));
    if(!isFinite(ms)){return;}
    var d=new Date(ms);
    el.textContent=d.getFullYear()+'-'+pad(d.getMonth()+1)+'-'+pad(d.getDate())+' '+pad(d.getHours())+':'+pad(d.getMinutes())+':'+pad(d.getSeconds());
  });
})();
</script>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot(attempted: u64, succeeded: u64) -> MetricsSnapshot {
        MetricsSnapshot {
            register_attempted: attempted,
            register_succeeded: succeeded,
            register_failed: attempted.saturating_sub(succeeded),
            register_success_rate: if attempted == 0 {
                0.0
            } else {
                succeeded as f64 / attempted as f64
            },
            heartbeat_succeeded: 120,
            heartbeat_failed: 3,
            active_streams: 40,
            fail_timeout: 2,
            fail_rejected: 1,
            fail_other: 0,
            position_reported: 8,
            alarm_reported: 1,
        }
    }

    fn record() -> RunRecord {
        let metrics = sample_snapshot(1000, 997);
        RunRecord {
            started_at_ms: 1_762_664_400_000,
            finished_at_ms: 1_762_665_000_000,
            device_count: 1000,
            target: "192.168.1.10:5060 · TCP".into(),
            gb_version: "GB/T 28181-2022".into(),
            scenario_rows: vec![
                ("设备名称".into(), "压测设备".into()),
                ("注册密码".into(), "已隐藏".into()),
            ],
            scenario_toml_redacted: "password = \"***\"\n".into(),
            samples: vec![
                Sample {
                    offset_secs: 0,
                    snapshot: sample_snapshot(100, 100),
                },
                Sample {
                    offset_secs: 600,
                    snapshot: metrics.clone(),
                },
            ],
            metrics,
        }
    }

    #[test]
    fn 转义特殊字符() {
        assert_eq!(esc("<b>&\"'"), "&lt;b&gt;&amp;&quot;&#39;");
    }

    #[test]
    fn 成功率分档() {
        assert_eq!(verdict(1.0, 10).0, "优秀");
        assert_eq!(verdict(0.995, 10).0, "良好");
        assert_eq!(verdict(0.96, 10).0, "需关注");
        assert_eq!(verdict(0.5, 10).0, "不达标");
        assert_eq!(verdict(0.0, 0).0, "数据不足");
    }

    #[test]
    fn 千分位与时长() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(1000), "1,000");
        assert_eq!(fmt_int(12_345_678), "12,345,678");
        assert_eq!(fmt_duration(600_000), "10 分 0 秒");
        assert_eq!(fmt_duration(3_725_000), "1 小时 2 分");
    }

    #[test]
    fn 报告含关键结论且不含明文密码() {
        let html = render_html(&record());
        assert!(html.starts_with("<!DOCTYPE html>"), "必须是完整 HTML 文档");
        assert!(html.contains("99.70%"), "应直接给出注册成功率");
        assert!(html.contains("1,000"), "设备数应带千分位");
        assert!(html.contains("超时无响应"), "应展示失败归因");
        assert!(html.contains("12 分 34 秒") || html.contains("10 分 0 秒"), "应给出运行时长");
        assert!(html.contains("<svg"), "应包含内联 SVG 图表");
        assert!(!html.contains("secret"), "报告不得出现明文密码");
        assert!(html.contains("password = &quot;***&quot;"), "TOML 需转义并脱敏");
    }

    #[test]
    fn 无采样时不画趋势图() {
        let mut r = record();
        r.samples.clear();
        let html = render_html(&r);
        assert!(html.contains("采样点不足") || html.contains("没有推流设备"));
        assert!(!html.contains("polyline points=\"0"));
    }

    #[test]
    fn 零失败走全通过分支() {
        let mut r = record();
        r.metrics = sample_snapshot(500, 500);
        let html = render_html(&r);
        assert!(html.contains("本次没有注册失败"), "零失败应给出正面结论");
        assert!(html.contains("没有出现注册失败"));
    }

    #[test]
    fn json_附属文件保留脱敏场景() {
        let json = render_json(&record());
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["device_count"], 1000);
        assert_eq!(value["metrics"]["register_succeeded"], 997);
        assert!(value["scenario_toml_redacted"].as_str().unwrap().contains("***"));
    }

    #[test]
    fn 抽稀保留末点() {
        let points: Vec<(u64, f64)> = (0..1000).map(|i| (i, i as f64)).collect();
        let out = decimate(points, 100);
        assert!(out.len() <= 101);
        assert_eq!(out.last().unwrap().0, 999);
    }

    /// 本地预览用:把一份带波动的样例报告写到临时目录,便于肉眼检查排版。
    #[test]
    #[ignore = "手工预览报告样式时运行"]
    fn dump_preview() {
        let mut r = record();
        r.samples = (0..180)
            .map(|i| {
                let attempted = 100 + i * 5;
                let succeeded = attempted - (i / 18) as u64;
                let mut snap = sample_snapshot(attempted, succeeded);
                snap.active_streams = if i < 30 {
                    i as u64 * 10
                } else if i < 120 {
                    300
                } else {
                    300 - (i - 120) as u64 * 2
                };
                Sample {
                    offset_secs: i * 2,
                    snapshot: snap,
                }
            })
            .collect();
        r.metrics = r.samples.last().unwrap().snapshot.clone();
        r.started_at_ms = 1_762_664_400_000;
        r.finished_at_ms = r.started_at_ms + 180 * 2 * 1000;
        let path = std::env::temp_dir().join("uvp-report-preview.html");
        std::fs::write(&path, render_html(&r)).unwrap();
        eprintln!("preview: {}", path.display());
    }
}
