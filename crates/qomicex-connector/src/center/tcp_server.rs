//! 联机中心 TCP 服务器：协议分发 + 心跳超时 + 连接生命周期。
//! 对应 C# `Qomicex.Connector/Center/TcpServer.cs`。

use std::collections::HashMap;
use std::sync::Arc;

use log::{error, info};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::center::client_conn::{ClientRegistry, handle_client, heartbeat_timeout_loop};
use crate::error::ScaffoldingError;
use crate::protocols::ProtocolHandler;
use crate::util::CancellationToken;

/// 联机中心 TCP 服务器。
pub struct TcpServer {
    /// 协议处理器表（对应 C# `_protocols.ToDictionary(p => p.ProtocolKey)`）。
    protocols: HashMap<String, Arc<dyn ProtocolHandler>>,
    /// 监听器；`None` 表示未启动。
    listener: Option<TcpListener>,
    /// 端口号（启动后更新为实际绑定端口）。
    port: u16,
    /// 服务器取消令牌（对应 C# `_cts`）。
    cts: CancellationToken,
    /// 连接管理共享状态（供 accept / 心跳 / 连接任务共享）。
    registry: ClientRegistry,
    /// 客户端断开事件发送端（对应 C# `ClientDisconnected`）。
    disconnected_tx: mpsc::UnboundedSender<String>,
}

impl TcpServer {
    /// 新建 TCP 服务器（对应 C# 构造函数；日志由 log crate 全局处理，故无 logger 参数）。
    ///
    /// `disconnected_tx` 为断开事件发送端：`Some(machine_id)` 发送 machine_id，
    /// `None` 发送空串（由接收方映射回 `None`）。
    pub fn new(
        port: u16,
        protocols: Vec<Arc<dyn ProtocolHandler>>,
        disconnected_tx: mpsc::UnboundedSender<String>,
    ) -> Self {
        let protocols = protocols
            .into_iter()
            .map(|p| (p.key().to_string(), p))
            .collect();
        Self {
            protocols,
            listener: None,
            port,
            cts: CancellationToken::new(),
            registry: ClientRegistry::new(),
            disconnected_tx,
        }
    }

    /// 是否正在运行（对应 C# `IsRunning`）。
    pub fn is_running(&self) -> bool {
        self.listener.is_some()
    }

    /// 实际绑定端口（对应 C# `Port`；启动前返回构造端口）。
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 启动服务并进入 accept 循环（对应 C# `StartAsync(ct)`）。
    ///
    /// 直接绑定 `0.0.0.0:port`（调用方 ScaffoldingCenter 已先做端口扫描传具体端口），
    /// 绑定后更新实际端口；随后并发运行心跳超时循环与 accept 循环，直到 `ct` 取消。
    pub async fn start(&mut self, ct: CancellationToken) -> Result<(), ScaffoldingError> {
        if self.listener.is_some() {
            return Err(ScaffoldingError::Protocol("TCP 服务已启动".to_string()));
        }
        self.cts = ct.clone();

        let listener = TcpListener::bind(std::net::SocketAddr::from(([0, 0, 0, 0], self.port)))
            .await
            .map_err(|e| {
                ScaffoldingError::Protocol(format!("TCP 端口 {} 绑定失败: {e}", self.port))
            })?;
        self.port = listener.local_addr().map(|a| a.port()).unwrap_or(self.port);
        info!("TCP 服务已启动，端口: {}", self.port);
        self.listener = Some(listener);

        tokio::spawn({
            let registry = self.registry.clone();
            let ct = ct.clone();
            async move {
                heartbeat_timeout_loop(registry, ct).await;
            }
        });

        let listener = self.listener.as_mut().expect("listener 已设置");
        loop {
            tokio::select! {
                _ = ct.cancelled() => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, addr)) => {
                            let client_id = addr.to_string();
                            info!("新客户端连接: {client_id}");
                            let conn_ct = CancellationToken::new();
                            let protocols = self.protocols.clone();
                            let registry = self.registry.clone();
                            let disconnected_tx = self.disconnected_tx.clone();
                            let server_ct = ct.clone();
                            tokio::spawn(async move {
                                handle_client(
                                    stream,
                                    client_id,
                                    protocols,
                                    registry,
                                    disconnected_tx,
                                    server_ct,
                                    conn_ct,
                                )
                                .await;
                            });
                        }
                        Err(e) => error!("接受连接失败: {e}"),
                    }
                }
            }
        }
        Ok(())
    }

    /// 停止服务（对应 C# `Stop()`）。
    ///
    /// 取消服务器令牌 → accept 循环、心跳循环与所有连接任务退出（流随之释放，
    /// 等价于 C# 直接 `Close()` 全部 `_activeClients`），并关闭监听器。
    pub fn stop(&mut self) {
        self.cts.cancel();
        self.listener.take();
        info!("TCP 服务已停止");
    }

    /// 按 machine_id 定向断开其 TCP 连接（踢人；未连接/未知 machine_id → false）。
    pub async fn disconnect_machine(&self, machine_id: &str) -> bool {
        self.registry.disconnect_machine(machine_id).await
    }
}
