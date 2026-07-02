//! GB/T 28181 应用层协议:MANSCDP XML 消息 + 国标编码规则。
//!
//! 承载在 SIP MESSAGE/INVITE 体内的 XML(目录、设备信息、报警、录像查询等),
//! 以及 20 位设备/通道 ID 的编码规则。纯数据编解码,不含网络与状态机。
//!
//! M0 为可编译骨架,`manscdp` / `id_codec` M1 起实现。

pub mod id_codec;
pub mod manscdp;
