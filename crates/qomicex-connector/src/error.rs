//! 异常体系：Scaffolding 错误枚举。

use std::fmt;

/// Scaffolding 错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaffoldingError {
    /// 无效的房间码格式（消息示例："无效的房间码格式: {code}"）。
    RoomCodeInvalid(String),
    /// EasyTier 启动失败 / 未找到可执行文件。
    EasyTierStart(String),
    /// EasyTier 启动超时（30s）。
    EasyTierTimeout(String),
    /// 未在 EasyTier 网络中发现联机中心（超时 30s）。
    CenterNotFound(String),
    /// 无法连接到联机中心（端口转发建立失败）。
    CenterConnection(String),
    /// 协议序列化 / 状态码错误。
    Protocol(String),
    /// 心跳超时。
    HeartbeatTimeout(String),
}

impl fmt::Display for ScaffoldingError {
    /// 直接输出携带的消息（与 C# Exception.Message 语义一致）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RoomCodeInvalid(msg)
            | Self::EasyTierStart(msg)
            | Self::EasyTierTimeout(msg)
            | Self::CenterNotFound(msg)
            | Self::CenterConnection(msg)
            | Self::Protocol(msg)
            | Self::HeartbeatTimeout(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ScaffoldingError {}
