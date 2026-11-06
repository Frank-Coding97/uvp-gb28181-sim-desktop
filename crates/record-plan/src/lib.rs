//! 录像计划（周时间表）模型与判定。
//!
//! 单独成 crate 的原因：这里全是纯逻辑（合并、切分、判定"此刻是否该录"），
//! 放在桌面端或模拟器里都会因为依赖 TLS 而无法在本机跑单测。定时录像是否按预期
//! 起停，全靠这些函数判断，必须有可执行的证据。
//!
//! 语义约定：
//! - 时间段左闭右开 `[start_sec, end_sec)`，所以 08:00-09:00 与 09:00-10:00
//!   相邻但不重叠，合并后是 08:00-10:00。
//! - 相邻段自动合并：界面按 30 分钟格子涂色，连点 4 格应当得到 1 段而不是 4 段。
//! - 每天最多 8 段（对齐国标设备与移动端能力），合并后再校验。

use serde::{Deserialize, Serialize};

/// 一天的秒数。
pub const SECS_PER_DAY: u32 = 86_400;
/// 网格粒度：30 分钟一格。
pub const BLOCK_SECS: u32 = 1_800;
/// 一天 48 格。
pub const BLOCKS_PER_DAY: usize = 48;
/// 每天最多计划段数。
pub const MAX_SEGMENTS_PER_DAY: usize = 8;

/// 一个时间段（左闭右开，秒为一天内偏移）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSegment {
    pub start_sec: u32,
    pub end_sec: u32,
}

impl PlanSegment {
    pub fn new(start_sec: u32, end_sec: u32) -> Self {
        Self { start_sec, end_sec }
    }

    /// 是否覆盖某一秒。左闭右开，避免相邻段在边界秒重复命中。
    pub fn contains(&self, sec: u32) -> bool {
        self.start_sec <= sec && sec < self.end_sec
    }

    pub fn duration_secs(&self) -> u32 {
        self.end_sec.saturating_sub(self.start_sec)
    }
}

/// 单日计划。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayPlan {
    #[serde(default)]
    pub segments: Vec<PlanSegment>,
}

impl DayPlan {
    pub fn covers(&self, sec: u32) -> bool {
        self.segments.iter().any(|segment| segment.contains(sec))
    }

    /// 是否整段被覆盖（用于判断格子该涂上还是擦掉）。
    pub fn covers_whole(&self, start_sec: u32, end_sec: u32) -> bool {
        let mut cursor = start_sec;
        while cursor < end_sec {
            if !self.covers(cursor) {
                return false;
            }
            // 直接跳到覆盖当前秒的那一段末尾，避免逐秒扫描。
            let next = self
                .segments
                .iter()
                .filter(|segment| segment.contains(cursor))
                .map(|segment| segment.end_sec)
                .max()
                .unwrap_or(cursor + 1);
            cursor = next.min(end_sec).max(cursor + 1);
        }
        true
    }

    /// 加入一段并归一化（排序 + 合并重叠/相邻 + 裁剪到当天）。
    pub fn add(&mut self, segment: PlanSegment) {
        self.segments.push(segment);
        self.normalize();
    }

    /// 从计划中减去一段（可能把一段切成两段）。
    pub fn subtract(&mut self, start_sec: u32, end_sec: u32) {
        if end_sec <= start_sec {
            return;
        }
        let mut next = Vec::with_capacity(self.segments.len() + 1);
        for segment in &self.segments {
            // 完全不相交：原样保留。
            if segment.end_sec <= start_sec || segment.start_sec >= end_sec {
                next.push(*segment);
                continue;
            }
            // 左侧残留。
            if segment.start_sec < start_sec {
                next.push(PlanSegment::new(segment.start_sec, start_sec));
            }
            // 右侧残留。
            if segment.end_sec > end_sec {
                next.push(PlanSegment::new(end_sec, segment.end_sec));
            }
        }
        self.segments = next;
        self.normalize();
    }

    /// 排序、裁剪、合并重叠与相邻段。
    pub fn normalize(&mut self) {
        let mut cleaned: Vec<PlanSegment> = self
            .segments
            .iter()
            .filter_map(|segment| {
                let start = segment.start_sec.min(SECS_PER_DAY);
                let end = segment.end_sec.min(SECS_PER_DAY);
                (end > start).then_some(PlanSegment::new(start, end))
            })
            .collect();
        cleaned.sort_by_key(|segment| (segment.start_sec, segment.end_sec));
        let mut merged: Vec<PlanSegment> = Vec::with_capacity(cleaned.len());
        for segment in cleaned {
            match merged.last_mut() {
                // `<=` 而不是 `<`：相邻段（前一秒结束、后一秒开始）也要合并。
                Some(last) if segment.start_sec <= last.end_sec => {
                    last.end_sec = last.end_sec.max(segment.end_sec);
                }
                _ => merged.push(segment),
            }
        }
        self.segments = merged;
    }
}

/// 周计划：周一 .. 周日（索引 0..6）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekPlan {
    #[serde(default)]
    pub days: Vec<DayPlan>,
}

impl Default for WeekPlan {
    fn default() -> Self {
        Self::empty()
    }
}

impl WeekPlan {
    /// 空计划（7 天全空）。
    pub fn empty() -> Self {
        Self {
            days: vec![DayPlan::default(); 7],
        }
    }

    /// 补齐到 7 天并逐日归一化。旧配置文件缺字段时用它兜底。
    pub fn normalized(mut self) -> Self {
        self.days.resize(7, DayPlan::default());
        self.days.truncate(7);
        for day in &mut self.days {
            day.normalize();
        }
        self
    }

    /// 校验（应先 `normalized`，否则重叠会被判为非法）。
    pub fn validate(&self) -> Result<(), String> {
        if self.days.len() != 7 {
            return Err(format!("周计划必须有 7 天，当前 {}", self.days.len()));
        }
        for (index, day) in self.days.iter().enumerate() {
            if day.segments.len() > MAX_SEGMENTS_PER_DAY {
                return Err(format!(
                    "周{}最多 {} 段，当前 {} 段",
                    weekday_label(index),
                    MAX_SEGMENTS_PER_DAY,
                    day.segments.len()
                ));
            }
            let mut previous_end = 0u32;
            for segment in &day.segments {
                if segment.start_sec >= segment.end_sec {
                    return Err(format!(
                        "周{}存在起止时间颠倒的时段 {} → {}",
                        weekday_label(index),
                        format_hms(segment.start_sec),
                        format_hms(segment.end_sec)
                    ));
                }
                if segment.end_sec > SECS_PER_DAY {
                    return Err(format!(
                        "周{}的时段超出 24 小时：{}",
                        weekday_label(index),
                        format_hms(segment.end_sec)
                    ));
                }
                if segment.start_sec < previous_end {
                    return Err(format!("周{}的时段有重叠，请先合并", weekday_label(index)));
                }
                previous_end = segment.end_sec;
            }
        }
        Ok(())
    }

    pub fn day(&self, weekday: usize) -> &DayPlan {
        // 索引越界时退化为"永不录像"，不 panic：定时任务不该因为一次脏数据把进程带崩。
        self.days.get(weekday).unwrap_or(&EMPTY_DAY)
    }

    /// 此刻是否应该按计划录像。
    pub fn should_record(&self, weekday: usize, sec: u32) -> bool {
        self.day(weekday).covers(sec)
    }

    /// 切换某个 30 分钟格子：整格已被覆盖则擦除，否则补上。
    pub fn toggle_block(&mut self, weekday: usize, block: usize) {
        if block >= BLOCKS_PER_DAY || weekday >= self.days.len() {
            return;
        }
        let start = block as u32 * BLOCK_SECS;
        let end = start + BLOCK_SECS;
        let day = &mut self.days[weekday];
        if day.covers_whole(start, end) {
            day.subtract(start, end);
        } else {
            day.add(PlanSegment::new(start, end));
        }
    }

    /// 某天的 48 格覆盖状态（界面涂色用）。
    pub fn blocks(&self, weekday: usize) -> Vec<bool> {
        let day = self.day(weekday);
        (0..BLOCKS_PER_DAY)
            .map(|block| {
                let start = block as u32 * BLOCK_SECS;
                day.covers_whole(start, start + BLOCK_SECS)
            })
            .collect()
    }

    pub fn total_segments(&self) -> usize {
        self.days.iter().map(|day| day.segments.len()).sum()
    }
}

static EMPTY_DAY: DayPlan = DayPlan {
    segments: Vec::new(),
};

/// 周几的中文标签（索引 0 = 周一）。
pub fn weekday_label(weekday: usize) -> &'static str {
    const LABELS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
    LABELS.get(weekday % 7).copied().unwrap_or("?")
}

/// 从"本地挂钟纪元秒"推出（周几, 当天秒偏移）。
///
/// 与 `common::clock` 的口径一致：入参是**校时后的挂钟纪元秒**，不是 UTC。
/// 1970-01-01 是周四，故周一为 0 的索引为 `(days + 3) mod 7`。
pub fn weekday_and_secs(epoch_secs: i64) -> (usize, u32) {
    let days = epoch_secs.div_euclid(i64::from(SECS_PER_DAY));
    let secs = epoch_secs.rem_euclid(i64::from(SECS_PER_DAY)) as u32;
    let weekday = (days + 3).rem_euclid(7) as usize;
    (weekday, secs)
}

/// `HH:MM:SS` 格式化；86400 显示为 `24:00:00`（时段终点允许写到当天末）。
pub fn format_hms(sec: u32) -> String {
    let sec = sec.min(SECS_PER_DAY);
    format!("{:02}:{:02}:{:02}", sec / 3_600, (sec % 3_600) / 60, sec % 60)
}

/// 解析 `H:MM`、`HH:MM:SS`、`24:00` 等写法。返回一天内秒偏移。
pub fn parse_hms(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut parts = text.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = match parts.next() {
        Some(value) => value.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    if minute > 59 || second > 59 {
        return None;
    }
    let total = hour * 3_600 + minute * 60 + second;
    (total <= SECS_PER_DAY).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_segments_merge() {
        let mut day = DayPlan::default();
        day.add(PlanSegment::new(8 * 3600, 9 * 3600));
        day.add(PlanSegment::new(9 * 3600, 10 * 3600));
        assert_eq!(day.segments, vec![PlanSegment::new(8 * 3600, 10 * 3600)]);
    }

    #[test]
    fn overlapping_segments_merge_and_keep_outer_bounds() {
        let mut day = DayPlan::default();
        day.add(PlanSegment::new(8 * 3600, 10 * 3600));
        day.add(PlanSegment::new(9 * 3600, 11 * 3600));
        assert_eq!(day.segments, vec![PlanSegment::new(8 * 3600, 11 * 3600)]);
    }

    #[test]
    fn subtract_splits_segment() {
        let mut day = DayPlan::default();
        day.add(PlanSegment::new(8 * 3600, 12 * 3600));
        day.subtract(9 * 3600, 10 * 3600);
        assert_eq!(
            day.segments,
            vec![
                PlanSegment::new(8 * 3600, 9 * 3600),
                PlanSegment::new(10 * 3600, 12 * 3600)
            ]
        );
    }

    #[test]
    fn toggle_block_paints_and_erases() {
        let mut plan = WeekPlan::empty();
        plan.toggle_block(0, 16); // 周一 08:00-08:30
        assert!(plan.should_record(0, 8 * 3600));
        assert!(!plan.should_record(0, 8 * 3600 + 1800), "右开区间，8:30 不属于该段");
        assert_eq!(plan.day(0).segments.len(), 1);

        plan.toggle_block(0, 17); // 08:30-09:00 应与上一格合并
        assert_eq!(
            plan.day(0).segments,
            vec![PlanSegment::new(8 * 3600, 9 * 3600)],
            "相邻格子必须合并成一段"
        );

        plan.toggle_block(0, 16); // 擦掉 08:00-08:30，只剩 08:30-09:00
        assert_eq!(
            plan.day(0).segments,
            vec![PlanSegment::new(8 * 3600 + 1800, 9 * 3600)]
        );
        assert!(!plan.should_record(0, 8 * 3600));
        assert!(plan.should_record(0, 8 * 3600 + 1800));
    }

    #[test]
    fn block_view_reflects_partial_coverage() {
        let mut plan = WeekPlan::empty();
        // 手工塞一个跨格时段：08:10-08:40，两个格子都不是"整格覆盖"。
        plan.days[0].add(PlanSegment::new(8 * 3600 + 600, 8 * 3600 + 2400));
        let blocks = plan.blocks(0);
        assert!(!blocks[16], "08:00 格只有部分覆盖，不应显示为满格");
        assert!(!blocks[17], "08:30 格同理");
        assert!(plan.should_record(0, 8 * 3600 + 700));
    }

    #[test]
    fn validates_day_count_and_segment_cap() {
        let mut plan = WeekPlan::empty();
        for hour in 0..9 {
            plan.days[0].segments.push(PlanSegment::new(hour * 3600, hour * 3600 + 1800));
        }
        let error = plan.validate().expect_err("超过 8 段应被拒绝");
        assert!(error.contains("最多"), "实际: {error}");

        let mut wrong = WeekPlan::empty();
        wrong.days.pop();
        assert!(wrong.validate().is_err());
    }

    #[test]
    fn rejects_overlapping_raw_segments() {
        let mut plan = WeekPlan::empty();
        plan.days[3].segments = vec![
            PlanSegment::new(8 * 3600, 10 * 3600),
            PlanSegment::new(9 * 3600, 11 * 3600),
        ];
        assert!(plan.validate().is_err(), "未归一化的重叠必须被拒绝");
        assert!(plan.clone().normalized().validate().is_ok());
    }

    #[test]
    fn rejects_reversed_and_out_of_range_segments() {
        let mut plan = WeekPlan::empty();
        plan.days[0].segments = vec![PlanSegment::new(3600, 3600)];
        assert!(plan.validate().is_err(), "空时段应被拒绝");

        let mut plan = WeekPlan::empty();
        plan.days[0].segments = vec![PlanSegment::new(3600, SECS_PER_DAY + 1)];
        assert!(plan.validate().is_err(), "越界应被拒绝");
    }

    #[test]
    fn weekday_mapping_matches_calendar() {
        // 2026-11-06 是周五（周一索引 4）。
        let epoch = common_epoch(2026, 11, 6, 13, 43, 7);
        let (weekday, secs) = weekday_and_secs(epoch);
        assert_eq!(weekday, 4, "2026-11-06 应为周五");
        assert_eq!(secs, 13 * 3600 + 43 * 60 + 7);
        // 1970-01-01 是周四。
        assert_eq!(weekday_and_secs(0).0, 3);
    }

    #[test]
    fn hms_roundtrip_and_edges() {
        assert_eq!(parse_hms("08:30"), Some(8 * 3600 + 1800));
        assert_eq!(parse_hms("8:30:15"), Some(8 * 3600 + 1800 + 15));
        assert_eq!(parse_hms("24:00:00"), Some(SECS_PER_DAY));
        assert_eq!(parse_hms("24:00:01"), None);
        assert_eq!(parse_hms("08:60"), None);
        assert_eq!(parse_hms(""), None);
        assert_eq!(format_hms(8 * 3600 + 1800), "08:30:00");
        assert_eq!(format_hms(SECS_PER_DAY), "24:00:00");
    }

    /// 测试里需要把公历日期换成纪元秒；不引入 chrono，直接用小算法。
    fn common_epoch(
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
    ) -> i64 {
        let y = if month <= 2 { year - 1 } else { year };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = (month + 9) % 12;
        let doy = (153 * mp + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        days * 86_400 + hour * 3600 + minute * 60 + second
    }
}
