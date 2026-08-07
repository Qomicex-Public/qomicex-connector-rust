//! 联机中心 TCP 客户端：连接 + 单发单收 + 15s 读超时。
//!
//! 对应 C# `Qomicex.Connector/Guest/TcpClient.cs`。

use std::time::Duration;

use log::{debug, error, info, warn};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::core::protocol_serializer::{deserialize_response_async, serialize_request};
use crate::error::ScaffoldingError;
use crate::models::protocol::{ProtocolRequest, ProtocolResponse};

/// 响应读取超时（对应 C# `NetworkStream.ReadTimeout = 15000`）。
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// 联机中心 TCP 客户端。
pub struct TcpClient {
    /// 当前连接流；`None` 表示未连接。
    stream: Option<TcpStream>,
    /// 发送锁（对应 C# `SemaphoreSlim(1, 1)`，保证单发单收）。
    send_lock: tokio::sync::Mutex<()>,
}

impl TcpClient {
    /// 新建客户端。
    ///
    /// C# 构造函数注入 `ILogger<TcpClient>`，Rust 侧由 log crate 全局处理，故无参数。
    pub fn new() -> Self {
        Self {
            stream: None,
            send_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 连接联机中心。
    ///
    /// 对应 C# `ConnectAsync(host, port, ct)`；取消参数按迁移决策移除
    /// （由调用方用 `tokio::select!` 包裹实现取消）。
    /// 连接失败 → `ScaffoldingError::CenterConnection`。
    pub async fn connect(&mut self, host: &str, port: u16) -> Result<(), ScaffoldingError> {
        let stream = TcpStream::connect((host, port)).await.map_err(|e| {
            ScaffoldingError::CenterConnection(format!("无法连接到联机中心 {host}:{port}: {e}"))
        })?;
        self.stream = Some(stream);
        info!("已连接到中心: {host}:{port}");
        Ok(())
    }

    /// 发送请求并读取响应（单发单收，响应读取 15s 超时）。
    ///
    /// 对应 C# `SendAsync(request, ct)`；取消参数按迁移决策移除。
    /// 未连接 → `CenterConnection("未连接到联机中心")`；
    /// 写失败 / 读超时 → `CenterConnection`；反序列化错误原样传播。
    pub async fn send(
        &mut self,
        request: &ProtocolRequest,
    ) -> Result<ProtocolResponse, ScaffoldingError> {
        let key = format!("{}:{}", request.namespace, request.request_type);
        if self.stream.is_none() {
            return Err(ScaffoldingError::CenterConnection(
                "未连接到联机中心".to_string(),
            ));
        }

        info!("发送: {key}, {} 字节", request.body.len());

        let _guard = self.send_lock.lock().await;
        let result = async {
            let stream = self
                .stream
                .as_mut()
                .expect("stream 非空已在上方检查");
            let request_bytes = serialize_request(request);
            debug!("发送原始数据: {} 字节", request_bytes.len());
            stream
                .write_all(&request_bytes)
                .await
                .map_err(|e| ScaffoldingError::CenterConnection(format!("TCP 发送失败: {key}: {e}")))?;
            stream
                .flush()
                .await
                .map_err(|e| ScaffoldingError::CenterConnection(format!("TCP 发送失败: {key}: {e}")))?;
            let response = tokio::time::timeout(READ_TIMEOUT, deserialize_response_async(stream))
                .await
                .map_err(|_| {
                    ScaffoldingError::CenterConnection(format!("读取响应超时: {key}"))
                })??;
            if !response.is_success() {
                warn!("请求 {key} 返回错误状态: {}", response.status);
            }
            Ok::<ProtocolResponse, ScaffoldingError>(response)
        }
        .await;

        if let Err(e) = &result {
            error!("TCP 发送/接收失败: {key}: {e}");
        }
        result
    }

    /// 是否已连接。
    ///
    /// 对应 C# `IsConnected`（检查 `_client.Connected`）；Rust 侧仅判断是否持有连接流。
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// 断开连接（对应 C# `Disconnect()`：释放流并记录日志）。
    pub fn disconnect(&mut self) {
        if self.stream.is_some() {
            self.stream = None;
            info!("已断开连接");
        }
    }
}

impl Drop for TcpClient {
    /// 对应 C# `Dispose()` → `Disconnect()`；已断开时不重复记录日志。
    fn drop(&mut self) {
        if self.stream.is_some() {
            self.stream = None;
            info!("已断开连接");
        }
    }
}
