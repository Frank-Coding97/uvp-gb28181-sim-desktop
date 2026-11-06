//! 最小 SNTP(RFC 5905)客户端。
//!
//! 只做一件事:向上级 NTP 服务器发起一次 48 字节查询,算出「服务器时间 − 本地时间」
//! 的偏移与往返时延。不修改操作系统时钟 —— 与 [`crate::clock`] 的定位一致:
//! 模拟器只维护逻辑协议时钟。
//!
//! 之所以自己实现而不引第三方 crate:报文格式固定、校验点少,自己写能把
//! 「拒绝不可信应答」的判据(模式/层级/发源时间戳)显式测出来。

use std::net::{ToSocketAddrs, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// NTP 纪元(1900-01-01)到 UNIX 纪元(1970-01-01)的秒数。
pub const NTP_UNIX_EPOCH_OFFSET_SECS: i64 = 2_208_988_800;
/// SNTP 报文固定长度。
const PACKET_LEN: usize = 48;
/// 发源时间戳允许的本地时钟抖动(毫秒)。
const ORIGINATE_TOLERANCE_MS: i64 = 1_000;

/// 一次成功的 SNTP 采样。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtpSample {
    /// 服务器时间 − 本地时间(毫秒)。
    pub offset_ms: i64,
    /// 往返时延(毫秒)。
    pub round_trip_ms: i64,
    /// 服务器层级(1~15;1 为最高精度)。
    pub stratum: u8,
    /// 实际查询的 `host:port`,用于界面回显。
    pub server: String,
}

impl NtpSample {
    /// 偏移换算成秒(四舍五入),供 GB28181 秒级时间戳使用。
    pub fn offset_secs(&self) -> i64 {
        round_div(self.offset_ms, 1_000)
    }
}

/// 发起一次 SNTP 查询(阻塞,调用方负责放进阻塞线程池)。
pub fn query(server: &str, port: u16, timeout: Duration) -> Result<NtpSample, String> {
    let server = server.trim();
    if server.is_empty() {
        return Err("NTP 服务器地址为空".into());
    }
    if port == 0 {
        return Err("NTP 端口必须在 1~65535".into());
    }
    let target = (server, port)
        .to_socket_addrs()
        .map_err(|error| format!("解析 NTP 服务器 {server}:{port} 失败: {error}"))?
        .next()
        .ok_or_else(|| format!("NTP 服务器 {server}:{port} 没有可用地址"))?;

    let socket = UdpSocket::bind(("0.0.0.0", 0))
        .map_err(|error| format!("创建 NTP socket 失败: {error}"))?;
    socket
        .connect(target)
        .map_err(|error| format!("连接 NTP 服务器 {server}:{port} 失败: {error}"))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("设置 NTP 超时失败: {error}"))?;

    let t1 = now_ms();
    socket
        .send(&build_request(t1))
        .map_err(|error| format!("发送 NTP 请求失败: {error}"))?;
    let mut buffer = [0u8; PACKET_LEN];
    let read = socket
        .recv(&mut buffer)
        .map_err(|error| format!("等待 NTP 应答失败: {error}"))?;
    let t4 = now_ms();

    let mut sample = parse_response(&buffer[..read], t1, t4)?;
    sample.server = format!("{server}:{port}");
    Ok(sample)
}

/// 构造客户端请求:LI=0 / VN=4 / Mode=3,并在 Transmit 字段写入本地发送时刻。
pub fn build_request(transmit_ms: i64) -> [u8; PACKET_LEN] {
    let mut packet = [0u8; PACKET_LEN];
    packet[0] = 0x23; // 0b00_100_011:LI=0, VN=4, Mode=3(client)
    write_timestamp(&mut packet, 40, transmit_ms);
    packet
}

/// 解析服务器应答。任何一项校验不通过都返回 `Err`,不做「尽力而为」的猜测。
pub fn parse_response(packet: &[u8], t1_ms: i64, t4_ms: i64) -> Result<NtpSample, String> {
    if packet.len() < PACKET_LEN {
        return Err(format!(
            "NTP 应答长度不足:{}(需要 {PACKET_LEN})",
            packet.len()
        ));
    }
    let leap = (packet[0] >> 6) & 0x03;
    if leap == 3 {
        return Err("NTP 服务器自报未同步(leap=3)".into());
    }
    let mode = packet[0] & 0x07;
    if mode != 4 && mode != 5 {
        return Err(format!("NTP 应答模式异常:{mode}(应为 4/5)"));
    }
    let stratum = packet[1];
    if !(1..=15).contains(&stratum) {
        return Err(format!("NTP 服务器层级异常:{stratum}(应为 1~15)"));
    }

    let originate = read_timestamp(packet, 24);
    if (originate - t1_ms).abs() > ORIGINATE_TOLERANCE_MS {
        return Err("NTP 应答的发起时间戳与本地请求不匹配".into());
    }
    let t2 = read_timestamp(packet, 32);
    let t3 = read_timestamp(packet, 40);
    if t2 <= 0 || t3 <= 0 {
        return Err("NTP 应答缺少有效时间戳".into());
    }

    let offset_ms = ((t2 - t1_ms) + (t3 - t4_ms)) / 2;
    let round_trip_ms = ((t4_ms - t1_ms) - (t3 - t2)).max(0);
    Ok(NtpSample {
        offset_ms,
        round_trip_ms,
        stratum,
        server: String::new(),
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// 32.32 定点时间戳 → UNIX 毫秒。
fn read_timestamp(packet: &[u8], offset: usize) -> i64 {
    let seconds = u32::from_be_bytes([
        packet[offset],
        packet[offset + 1],
        packet[offset + 2],
        packet[offset + 3],
    ]) as i64;
    let fraction = u32::from_be_bytes([
        packet[offset + 4],
        packet[offset + 5],
        packet[offset + 6],
        packet[offset + 7],
    ]) as u64;
    // 小数部分按四舍五入还原成毫秒：直接右移会把 100ms 读成 99ms，
    // 两次采样相减后误差会放大到秒级偏移上。
    let fraction_ms = ((fraction * 1_000 + (1u64 << 31)) >> 32) as i64;
    (seconds - NTP_UNIX_EPOCH_OFFSET_SECS) * 1_000 + fraction_ms
}

/// UNIX 毫秒 → 32.32 定点时间戳。
fn write_timestamp(packet: &mut [u8], offset: usize, epoch_ms: i64) {
    let seconds = epoch_ms.div_euclid(1_000) + NTP_UNIX_EPOCH_OFFSET_SECS;
    let millis = epoch_ms.rem_euclid(1_000) as u64;
    let fraction = ((millis * (1u64 << 32) + 500) / 1_000) as u32;
    packet[offset..offset + 4].copy_from_slice(&(seconds as u32).to_be_bytes());
    packet[offset + 4..offset + 8].copy_from_slice(&fraction.to_be_bytes());
}

fn round_div(value: i64, unit: i64) -> i64 {
    if value >= 0 {
        (value + unit / 2) / unit
    } else {
        -((-value + unit / 2) / unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一份合法应答:t1 由调用方给出,t2/t3 由测试指定。
    fn response(t1_ms: i64, t2: i64, t3: i64) -> [u8; PACKET_LEN] {
        let mut packet = [0u8; PACKET_LEN];
        packet[0] = 0x24; // LI=0, VN=4, Mode=4(server)
        packet[1] = 2; // stratum 2
        write_timestamp(&mut packet, 24, t1_ms);
        write_timestamp(&mut packet, 32, t2);
        write_timestamp(&mut packet, 40, t3);
        packet
    }

    #[test]
    fn 偏移与往返时延计算() {
        let t1 = 1_700_000_000_000;
        // 服务器收 100ms、发 110ms,本地收 130ms → offset=((100)+(110-130))/2=40ms
        let packet = response(t1, t1 + 100, t1 + 110);
        let sample = parse_response(&packet, t1, t1 + 130).unwrap();
        assert_eq!(sample.offset_ms, 40);
        assert_eq!(sample.round_trip_ms, 120);
        assert_eq!(sample.stratum, 2);
        assert_eq!(sample.offset_secs(), 0);
    }

    #[test]
    fn 时间戳往返无损到毫秒() {
        let mut packet = [0u8; PACKET_LEN];
        write_timestamp(&mut packet, 40, 1_700_000_123_456);
        let decoded = read_timestamp(&packet, 40);
        assert!(
            (decoded - 1_700_000_123_456).abs() <= 1,
            "decoded={decoded}"
        );
    }

    #[test]
    fn 拒绝未同步服务器() {
        let t1 = 1_700_000_000_000;
        let mut packet = response(t1, t1, t1);
        packet[0] = 0xE4; // leap=3
        assert!(parse_response(&packet, t1, t1 + 10).is_err());
    }

    #[test]
    fn 拒绝错误模式与层级() {
        let t1 = 1_700_000_000_000;
        let mut packet = response(t1, t1, t1);
        packet[0] = 0x23; // Mode=3(client)
        assert!(parse_response(&packet, t1, t1 + 10).is_err());

        let mut packet = response(t1, t1, t1);
        packet[1] = 0; // kiss-o'-death
        assert!(parse_response(&packet, t1, t1 + 10).is_err());
    }

    #[test]
    fn 拒绝发源时间戳不匹配的应答() {
        let t1 = 1_700_000_000_000;
        let packet = response(t1 + 5_000, t1 + 5_100, t1 + 5_110);
        assert!(parse_response(&packet, t1, t1 + 5_130).is_err());
    }

    #[test]
    fn 拒绝过短报文() {
        assert!(parse_response(&[0u8; 20], 0, 10).is_err());
    }

    #[test]
    fn 请求报文头与发源时间戳() {
        let t1 = 1_700_000_000_000;
        let packet = build_request(t1);
        assert_eq!(packet[0], 0x23);
        assert!((read_timestamp(&packet, 40) - t1).abs() <= 1);
    }

    #[test]
    fn 秒级四舍五入() {
        assert_eq!(round_div(1_400, 1_000), 1);
        assert_eq!(round_div(1_500, 1_000), 2);
        assert_eq!(round_div(-1_500, 1_000), -2);
        assert_eq!(round_div(-1_400, 1_000), -1);
    }

    #[test]
    fn 空服务器与非法端口直接报错() {
        assert!(query("  ", 123, Duration::from_millis(10)).is_err());
        assert!(query("127.0.0.1", 0, Duration::from_millis(10)).is_err());
    }
}
