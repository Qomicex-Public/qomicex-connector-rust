//! Core 层：协议编解码、协议协商、联机中心发现与心跳服务。

pub mod center_discovery;
pub mod heartbeat;
pub mod protocol_negotiator;
pub mod protocol_serializer;
