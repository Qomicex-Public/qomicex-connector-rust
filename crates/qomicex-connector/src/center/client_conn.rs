//! 客户端连接处理（对应 C# `TcpServer.HandleClientAsync` / `HeartbeatTimeoutLoop`）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::core::protocol_serializer::{deserialize_request_async, serialize_response};
use crate::models::protocol::ProtocolResponse;
use crate::protocols::ProtocolHandler;
use crate::util::CancellationToken;

/// 心跳检查周期（对应 C# `Task.Delay(TimeSpan.FromSeconds(5))`）。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// 心跳超时阈值（对应 C# 超过 15 秒判定超时）。
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);

/// 连接管理共享状态（对应 C# `_lastHeartbeat` / `_activeClients` / `_clientIdToMachineId`）。
///
/// 四个并发字典以 `Arc<tokio::sync::Mutex>` 包装，供 accept 循环、心跳循环与各连接任务共享。
#[derive(Clone)]
pub(crate) struct ClientRegistry {
    /// 最近心跳时间（client_id → Instant）。
    last_heartbeat: Arc<tokio::sync::Mutex<HashMap<String, Instant>>>,
    /// 存活连接（client_id → 连接取消令牌，供心跳超时定向断开）。
    active_clients: Arc<tokio::sync::Mutex<HashMap<String, CancellationToken>>>,
    /// 客户端映射（client_id → machine_id，来自 `c:player_ping` 请求体）。
    client_machine: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
    /// 客户端源 IP（client_id → 源 IP；踢人时按 machine_id 反查其 easytier peer）。
    client_ip: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
}

impl ClientRegistry {
    /// 新建空注册表。
    pub(crate) fn new() -> Self {
        Self {
            last_heartbeat: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            active_clients: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            client_machine: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            client_ip: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// 按 machine_id 定向断开其 TCP 连接（踢人）：找到对应 client_id 并取消其
    /// 连接令牌，连接任务随即退出并在清理段触发断开事件。返回是否找到连接。
    pub(crate) async fn disconnect_machine(&self, machine_id: &str) -> bool {
        let conn_ct = {
            let machine = self.client_machine.lock().await;
            let Some((client_id, _)) = machine
                .iter()
                .find(|(_, mid)| mid.as_str() == machine_id)
                .map(|(cid, mid)| (cid.clone(), mid.clone()))
            else {
                return false;
            };
            drop(machine);
            self.active_clients.lock().await.remove(&client_id)
        };
        match conn_ct {
            Some(ct) => {
                ct.cancel();
                true
            }
            None => false,
        }
    }

    /// 查询指定 machine_id 的 SCF TCP 连接源 IP（踢人反查其 easytier peer；
    /// 无连接/未知 machine_id → `None`）。
    pub(crate) async fn machine_source_ip(&self, machine_id: &str) -> Option<String> {
        let client_id = {
            let machine = self.client_machine.lock().await;
            machine
                .iter()
                .find(|(_, mid)| mid.as_str() == machine_id)
                .map(|(cid, _)| cid.clone())
        }?;
        self.client_ip.lock().await.get(&client_id).cloned()
    }
}

/// 处理单个客户端连接（对应 C# `TcpServer.HandleClientAsync`）。
///
/// 循环：读请求 → 按 `{namespace}:{request_type}` 查协议表 → 分发处理 → 写回响应；
/// 未命中协议 → status 255 + UTF8 体 `未知协议: {key}`。
/// `c:player_ping` 且成功时记录心跳时间，并解析请求体 JSON 提取 `machine_id`（手动取，容错）。
/// `server_ct`（服务停止）或 `conn_ct`（心跳超时）任一取消 → 退出循环 → 清理并触发断开事件。
pub(crate) async fn handle_client(
    mut stream: TcpStream,
    client_id: String,
    client_ip: String,
    protocols: HashMap<String, Arc<dyn ProtocolHandler>>,
    registry: ClientRegistry,
    disconnected_tx: mpsc::UnboundedSender<String>,
    server_ct: CancellationToken,
    conn_ct: CancellationToken,
) {
    registry
        .active_clients
        .lock()
        .await
        .insert(client_id.clone(), conn_ct.clone());
    registry
        .client_ip
        .lock()
        .await
        .insert(client_id.clone(), client_ip);

    let result: Result<(), String> = async {
        loop {
            tokio::select! {
                biased;
                _ = server_ct.cancelled() => break,
                _ = conn_ct.cancelled() => break,
                read = deserialize_request_async(&mut stream) => {
                    let request = match read {
                        Ok(request) => request,
                        Err(e) => return Err(e.to_string()),
                    };
                    let key = format!("{}:{}", request.namespace, request.request_type);
                    debug!("收到请求: {key} 来自 {client_id}");

                    let response = match protocols.get(&key) {
                        Some(handler) => {
                            let response = handler.handle(&request).await;
                            info!("处理请求: {key} -> 状态 {}", response.status);
                            response
                        }
                        None => ProtocolResponse {
                            status: 255,
                            body: format!("未知协议: {key}").into_bytes(),
                        },
                    };

                    let bytes = serialize_response(&response);
                    if let Err(e) = stream.write_all(&bytes).await {
                        return Err(e.to_string());
                    }

                    if key == "c:player_ping" && response.is_success() {
                        registry
                            .last_heartbeat
                            .lock()
                            .await
                            .insert(client_id.clone(), Instant::now());
                        if let Ok(root) = serde_json::from_slice::<serde_json::Value>(&request.body) {
                            if let Some(mid) = root.get("machine_id") {
                                let machine_id = mid.as_str().unwrap_or("").to_string();
                                registry
                                    .client_machine
                                    .lock()
                                    .await
                                    .insert(client_id.clone(), machine_id);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    if let Err(message) = result {
        warn!("客户端 {client_id} 处理异常: {message}");
    }

    registry.active_clients.lock().await.remove(&client_id);
    registry.last_heartbeat.lock().await.remove(&client_id);
    registry.client_ip.lock().await.remove(&client_id);
    let machine_id = registry.client_machine.lock().await.remove(&client_id);
    notify_disconnected(&disconnected_tx, machine_id);
}

/// 心跳超时循环（对应 C# `TcpServer.HeartbeatTimeoutLoop`）。
///
/// 每 5s 检查一次：超过 15s 未收到心跳的客户端，移除心跳记录并取消其连接令牌，
/// 连接任务随后退出并在清理段单次触发 `ClientDisconnected`（见移植日志决策 3）。
pub(crate) async fn heartbeat_timeout_loop(registry: ClientRegistry, ct: CancellationToken) {
    loop {
        tokio::select! {
            _ = ct.cancelled() => break,
            _ = tokio::time::sleep(HEARTBEAT_INTERVAL) => {
                let now = Instant::now();
                let timed_out: Vec<String> = {
                    let guard = registry.last_heartbeat.lock().await;
                    guard
                        .iter()
                        .filter(|(_, last)| now.duration_since(**last) > HEARTBEAT_TIMEOUT)
                        .map(|(id, _)| id.clone())
                        .collect()
                };
                for id in timed_out {
                    warn!("客户端 {id} 心跳超时，断开连接");
                    registry.last_heartbeat.lock().await.remove(&id);
                    if let Some(conn_ct) = registry.active_clients.lock().await.remove(&id) {
                        conn_ct.cancel();
                    }
                }
            }
        }
    }
}

/// 通知客户端断开（对应 C# `ClientDisconnected?.Invoke(machineId)`）。
///
/// 约定：`Some(machine_id)` 发送 machine_id；`None` 发送空串，由接收方映射回 `None`。
/// 发送失败仅记 debug 日志（对应 C# 事件无返回值、无错误处理）。
fn notify_disconnected(tx: &mpsc::UnboundedSender<String>, machine_id: Option<String>) {
    let message = machine_id.unwrap_or_default();
    if let Err(e) = tx.send(message) {
        debug!("客户端断开事件发送失败: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn machine_source_ip_resolves_from_registry() {
        let registry = ClientRegistry::new();
        // 模拟 guest 连接：client_id = "10.144.144.5:54321"，ping 上报 machine_id
        registry
            .client_machine
            .lock()
            .await
            .insert("10.144.144.5:54321".into(), "g1".into());
        registry
            .client_ip
            .lock()
            .await
            .insert("10.144.144.5:54321".into(), "10.144.144.5".into());

        assert_eq!(
            registry.machine_source_ip("g1").await.as_deref(),
            Some("10.144.144.5")
        );
        // 未知 machine_id / 未上报的客户端 → None
        assert_eq!(registry.machine_source_ip("unknown").await, None);
    }
}
