//! `Subject` 头域(GB/T 28181-2022 附录 L)。
//!
//! 格式:`Subject: 媒体流发送者ID:发送方媒体流序列号,媒体流接收者ID:接收方媒体流序列号`
//!
//! 为什么它不是可选的装饰:
//!
//! - **§9.2.3 c)**:发送给媒体服务器的消息头**应**包括 Subject 字段,系统应支持该字段。
//! - **附录 K c)**:设备收到呼叫请求后,应用"媒体流发送者 ID:发送方媒体流序列号"这个
//!   *媒体源标识* 判断自己是否已在发送同一路码流;**若已在发送,应释放现有链路并按新请求
//!   重建**。没有 Subject 就无从判断,只能退化成"有会话就回 486 Busy Here" ——
//!   那会让平台重启后永远拉不到流(旧会话在设备侧还挂着,新请求一律被拒)。
//!
//! 序列号规则(附录 L):不超过 20 位字符串;**实时流首位取 0,历史流首位取 1**,
//! 同一路流取值唯一。

use std::fmt;

/// 解析后的 Subject 头域。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    /// 媒体流发送者 ID(点播时是设备/通道 ID)。
    pub sender_id: String,
    /// 发送方媒体流序列号。
    pub sender_seq: String,
    /// 媒体流接收者 ID(点播时是平台/媒体服务器 ID)。
    pub receiver_id: String,
    /// 接收方媒体流序列号。
    pub receiver_seq: String,
}

impl Subject {
    /// 解析 `Subject` 头域值。任一段缺失即视为非法,返回 None。
    ///
    /// 宽容处理两点:段内首尾空白允许存在;接收方序列号可以为空串
    /// (部分平台只填三段),此时按空序列号保留而不是整体判非法 ——
    /// 媒体源标识只依赖前两段,不该被第四段的写法差异影响。
    pub fn parse(value: &str) -> Option<Self> {
        let (sender, receiver) = value.trim().split_once(',')?;
        let (sender_id, sender_seq) = sender.trim().split_once(':')?;
        let (receiver_id, receiver_seq) = receiver.trim().split_once(':')?;
        let sender_id = sender_id.trim();
        let receiver_id = receiver_id.trim();
        if sender_id.is_empty() || receiver_id.is_empty() || sender_seq.trim().is_empty() {
            return None;
        }
        Some(Subject {
            sender_id: sender_id.to_string(),
            sender_seq: sender_seq.trim().to_string(),
            receiver_id: receiver_id.to_string(),
            receiver_seq: receiver_seq.trim().to_string(),
        })
    }

    /// 媒体源标识 `发送者ID:发送方序列号`(附录 K c) 的比较依据)。
    pub fn media_source(&self) -> String {
        format!("{}:{}", self.sender_id, self.sender_seq)
    }

    /// 是否为历史媒体流(序列号首位为 1)。
    pub fn is_history(&self) -> bool {
        self.sender_seq.starts_with('1')
    }

    /// 构造实时流的 Subject(发送方序列号首位 0)。
    pub fn realtime(
        sender_id: &str,
        sender_seq: u32,
        receiver_id: &str,
        receiver_seq: u32,
    ) -> Self {
        Subject {
            sender_id: sender_id.to_string(),
            sender_seq: seq_string('0', sender_seq),
            receiver_id: receiver_id.to_string(),
            receiver_seq: seq_string('0', receiver_seq),
        }
    }

    /// 构造历史流的 Subject(发送方序列号首位 1)。
    pub fn history(sender_id: &str, sender_seq: u32, receiver_id: &str, receiver_seq: u32) -> Self {
        Subject {
            sender_id: sender_id.to_string(),
            sender_seq: seq_string('1', sender_seq),
            receiver_id: receiver_id.to_string(),
            receiver_seq: seq_string('0', receiver_seq),
        }
    }
}

/// 生成"首位固定 + 9 位零填充"的 10 位序列号(附录 L 上限 20 位)。
fn seq_string(first: char, value: u32) -> String {
    format!("{first}{value:09}")
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{},{}:{}",
            self.sender_id, self.sender_seq, self.receiver_id, self.receiver_seq
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析标准四段格式() {
        let s = Subject::parse("34020000001320000001:0000000001,34020000002000000001:0")
            .expect("应解析成功");
        assert_eq!(s.sender_id, "34020000001320000001");
        assert_eq!(s.sender_seq, "0000000001");
        assert_eq!(s.receiver_id, "34020000002000000001");
        assert_eq!(s.receiver_seq, "0");
        assert_eq!(s.media_source(), "34020000001320000001:0000000001");
        assert!(!s.is_history());
    }

    #[test]
    fn 历史流按序列号首位识别() {
        let s = Subject::parse("d:1000000007,p:0000000001").expect("parse");
        assert!(s.is_history(), "发送方序列号首位为 1 应识别为历史流");
    }

    #[test]
    fn 接收方序列号可为空() {
        // 部分平台把接收方序列号留空,媒体源标识不受影响,不应整体判非法。
        let s = Subject::parse("d:0000000001,p:").expect("parse");
        assert_eq!(s.receiver_seq, "");
        assert_eq!(s.media_source(), "d:0000000001");
    }

    #[test]
    fn 缺段或缺发送方序列号判非法() {
        assert!(Subject::parse("").is_none());
        assert!(Subject::parse("d:0000000001").is_none(), "缺接收方段");
        assert!(Subject::parse("d,p:0").is_none(), "发送方缺冒号");
        assert!(Subject::parse("d:,p:0").is_none(), "发送方序列号为空");
        assert!(Subject::parse(":1,p:0").is_none(), "发送方 ID 为空");
    }

    #[test]
    fn 构造与解析可往返() {
        let s = Subject::realtime("34020000001320000001", 7, "34020000002000000001", 1);
        assert_eq!(s.sender_seq, "0000000007");
        assert_eq!(
            s.to_string(),
            "34020000001320000001:0000000007,34020000002000000001:0000000001"
        );
        assert_eq!(Subject::parse(&s.to_string()).expect("round trip"), s);
        // 历史流首位 1。
        let h = Subject::history("d", 3, "p", 1);
        assert!(h.is_history());
        assert_eq!(h.sender_seq, "1000000003");
    }

    #[test]
    fn 序列号不超过20位() {
        let s = Subject::realtime("d", u32::MAX, "p", u32::MAX);
        assert!(s.sender_seq.len() <= 20, "实际 {}", s.sender_seq.len());
        assert!(s.receiver_seq.len() <= 20);
    }
}
