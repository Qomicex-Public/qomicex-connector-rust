//! 协议帧：请求与响应。

/// 协议请求帧。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtocolRequest {
    /// 命名空间。
    pub namespace: String,
    /// 请求类型。
    pub request_type: String,
    /// 请求体。
    pub body: Vec<u8>,
}

/// 协议响应帧。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtocolResponse {
    /// 状态码（0 表示成功）。
    pub status: u8,
    /// 响应体。
    pub body: Vec<u8>,
}

impl ProtocolResponse {
    /// 状态码是否为 0。
    pub fn is_success(&self) -> bool {
        self.status == 0
    }
}
