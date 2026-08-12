//! 访客端联机客户端（ScaffoldingGuest）。
//! 对应 C# `Qomicex.Connector/Guest/ScaffoldingGuest.cs`：
//! 加入 EasyTier 网络 → 发现联机中心 → 直连或端口转发 → 协议协商 → 心跳。
//!
//! 锁纪律：所有共享状态通过 `Arc<Mutex<...>>` 保护，**一次只持一个锁**，
//! 用完即释放（块作用域 / 语句结束即 drop），禁止同时持有两个锁，
//! 避免 async 环境下的锁重入死锁。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::core::center_discovery::{discover, CenterDiscoveryResult};
use crate::core::heartbeat::HeartbeatService;
use crate::core::protocol_negotiator::negotiate;
use crate::easytier::EasyTierManager;
use crate::error::ScaffoldingError;
use crate::guest::tcp_client::TcpClient;
use crate::models::network_config::NetworkConfig;
use crate::models::player::{PlayerInfo, PlayerKind, PlayerProfileEntry};
use crate::models::protocol::{ProtocolRequest, ProtocolResponse};
use crate::models::room_code::RoomCode;
use crate::util::CancellationToken;

/// 标准协议集合（对齐 C# `NegotiateProtocolsAsync` 中的硬编码列表）。
const STANDARD_PROTOCOLS: [&str; 6] = [
    "c:ping",
    "c:protocols",
    "c:server_port",
    "c:player_ping",
    "c:player_profiles_list",
    "c:player_easytier_id",
];

/// 心跳间隔（对齐 C# `HeartbeatService.StartAsync(5s)`）。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// 单次连接尝试超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// 重试间隔。
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// 访客端联机客户端。
pub struct ScaffoldingGuest {
    /// 玩家名。
    player_name: String,
    /// 机器标识。
    machine_id: String,
    /// 启动器厂商。
    vendor: String,
    /// EasyTier 管理器。
    easy_tier: Arc<tokio::sync::Mutex<EasyTierManager>>,
    /// 联机中心 TCP 客户端。
    tcp: Arc<tokio::sync::Mutex<TcpClient>>,
    /// 自定义协议键列表。
    custom_protocol_keys: Vec<String>,
    /// 中继节点列表（可选）。
    relay_nodes: Option<Vec<String>>,
    /// 本次联机协商出的可用协议列表。
    negotiated: Arc<tokio::sync::RwLock<Vec<String>>>,
    /// 当前网络配置。
    config: tokio::sync::Mutex<Option<NetworkConfig>>,
    /// 联机中心发现结果。
    center: tokio::sync::Mutex<Option<CenterDiscoveryResult>>,
    /// 是否使用端口转发模式。
    use_port_forward: tokio::sync::Mutex<bool>,
    /// MC 服务器可连接地址（`map_minecraft_port` 后可用）。
    minecraft_host: tokio::sync::Mutex<Option<String>>,
    /// MC 服务器可连接端口（`map_minecraft_port` 后可用）。
    minecraft_port: tokio::sync::Mutex<Option<u16>>,
    /// 当前心跳任务的取消令牌（`Some` 表示心跳运行中）。
    heartbeat_ct: tokio::sync::Mutex<Option<CancellationToken>>,
    /// 连接丢失通知发送端（心跳失败 / 未连接时触发）。
    connection_lost_tx: tokio::sync::watch::Sender<bool>,
}

impl ScaffoldingGuest {
    /// 创建访客端客户端。
    pub fn new(
        player_name: String,
        machine_id: String,
        vendor: String,
        custom_protocol_keys: Vec<String>,
        relay_nodes: Option<Vec<String>>,
    ) -> Self {
        Self {
            player_name,
            machine_id,
            vendor,
            easy_tier: Arc::new(tokio::sync::Mutex::new(EasyTierManager::new())),
            tcp: Arc::new(tokio::sync::Mutex::new(TcpClient::new())),
            custom_protocol_keys,
            relay_nodes,
            negotiated: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            config: tokio::sync::Mutex::new(None),
            center: tokio::sync::Mutex::new(None),
            use_port_forward: tokio::sync::Mutex::new(false),
            minecraft_host: tokio::sync::Mutex::new(None),
            minecraft_port: tokio::sync::Mutex::new(None),
            heartbeat_ct: tokio::sync::Mutex::new(None),
            connection_lost_tx: tokio::sync::watch::channel(false).0,
        }
    }

    /// 连接丢失通知接收端（值变为 `true` 表示连接已断开）。
    pub fn connection_lost_rx(&self) -> tokio::sync::watch::Receiver<bool> {
        self.connection_lost_tx.subscribe()
    }

    /// 本次联机协商出的可用协议列表（含标准协议与双端都支持的自定义协议）。
    pub async fn negotiated_protocols(&self) -> Vec<String> {
        self.negotiated.read().await.clone()
    }

    /// MC 服务器可连接地址（调用 `map_minecraft_port` 后可用）。
    pub async fn minecraft_host(&self) -> Option<String> {
        self.minecraft_host.lock().await.clone()
    }

    /// MC 服务器可连接端口（调用 `map_minecraft_port` 后可用）。
    pub async fn minecraft_port(&self) -> Option<u16> {
        *self.minecraft_port.lock().await
    }

    /// 加入房间：启动 EasyTier → 发现联机中心 → 直连或端口转发 → 协议协商 → 启动心跳。
    pub async fn connect(
        &self,
        code: &RoomCode,
        ct: CancellationToken,
    ) -> Result<(), ScaffoldingError> {
        // 字符切片（非 ASCII machine_id 安全；对应 C# `MachineId[..Math.Min(8, Length)]`）
        let hostname_suffix: String = self.machine_id.chars().take(8).collect();
        // 管理员 → TUN 虚拟网卡（wintun）；非管理员 → no-tun（smoltcp 用户态栈）
        let elevated = crate::util::is_elevated();
        let config = NetworkConfig {
            network_name: code.easy_tier_network_name(),
            network_secret: code.easy_tier_network_secret().to_string(),
            hostname: format!("scaffolding-mc-guest-{hostname_suffix}"),
            no_tun: !elevated,
            use_smoltcp: !elevated,
            dhcp: true,
            relay_nodes: self.relay_nodes.clone(),
            bind_ip: crate::util::resolve_bind_ip(),
            ..Default::default()
        };

        self.easy_tier.lock().await.start(&config).await?;
        log::info!("已加入 EasyTier 网络");

        let et = self.easy_tier.clone();
        let center = discover(
            move || {
                let et = et.clone();
                async move { et.lock().await.get_nodes().await }
            },
            &ct,
        )
        .await?;
        *self.center.lock().await = Some(center.clone());
        *self.config.lock().await = Some(config.clone());

        // 先尝试直连虚拟 IP（有 TUN 权限时成功，无需重启）
        if self
            .try_connect_once(&center.virtual_ip, center.port, CONNECT_TIMEOUT)
            .await?
        {
            *self.use_port_forward.lock().await = false;
            log::info!("虚拟 IP 直连成功，无需端口转发");
        } else {
            // 失败则动态添加 Center port-forward（管理 RPC 热更新，不重启实例：
            // 重启会重建虚拟网络，重试窗口内路由未恢复 → 转发连不上 Center）
            let local_port = find_free_local_port().await;
            log::info!(
                "虚拟 IP 不可达，建立端口转发 127.0.0.1:{local_port} -> {}:{}",
                center.virtual_ip,
                center.port
            );
            let forward = format!(
                "tcp://127.0.0.1:{local_port}/{}:{}",
                center.virtual_ip, center.port
            );
            self.easy_tier.lock().await.add_port_forward(&forward).await?;
            {
                let mut cfg = self.config.lock().await;
                if let Some(c) = cfg.as_mut() {
                    c.port_forwards.push(forward);
                }
            }

            if !self
                .try_connect_with_retry("127.0.0.1", local_port, 10, ct.clone())
                .await?
            {
                return Err(ScaffoldingError::CenterConnection(
                    "无法连接到联机中心（端口转发建立失败）".into(),
                ));
            }
            *self.use_port_forward.lock().await = true;
        }

        log::info!("已连接联机中心，启动心跳");
        self.start_heartbeat(ct).await;
        Ok(())
    }

    /// 单次连接尝试：连接 → 发送玩家 ping → 协商协议，全部成功才返回 `true`。
    async fn try_connect_once(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<bool, ScaffoldingError> {
        let ok = tokio::time::timeout(timeout, self.tcp.lock().await.connect(host, port)).await;
        match ok {
            Ok(Ok(())) => {
                if self.send_player_ping().await.is_ok() && self.negotiate_protocols().await.is_ok()
                {
                    Ok(true)
                } else {
                    self.tcp.lock().await.disconnect();
                    Ok(false)
                }
            }
            Ok(Err(e)) => {
                log::debug!("连接 {host}:{port} 失败: {e}");
                self.tcp.lock().await.disconnect();
                Ok(false)
            }
            Err(_) => {
                log::debug!("连接 {host}:{port} 超时");
                self.tcp.lock().await.disconnect();
                Ok(false)
            }
        }
    }

    /// 多次重试连接：给 EasyTier P2P 打洞留时间（两端 PortRestricted NAT 打洞需要多轮探测）。
    async fn try_connect_with_retry(
        &self,
        host: &str,
        port: u16,
        retries: u32,
        ct: CancellationToken,
    ) -> Result<bool, ScaffoldingError> {
        for i in 1..=retries {
            if ct.is_cancelled() {
                return Ok(false);
            }
            log::info!("尝试连接中心 {host}:{port} (第 {i}/{retries} 次)");
            if self
                .try_connect_once(host, port, CONNECT_TIMEOUT)
                .await?
            {
                return Ok(true);
            }
            if i < retries {
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
        Ok(false)
    }

    /// 发送玩家资料 ping（`c:player_ping`）；未连接时触发连接丢失通知。
    async fn send_player_ping(&self) -> Result<(), ScaffoldingError> {
        let connected = self.tcp.lock().await.is_connected();
        if !connected {
            let _ = self.connection_lost_tx.send(true);
            return Ok(());
        }

        let has_easytier_id = self
            .negotiated
            .read()
            .await
            .contains(&"c:player_easytier_id".to_string());
        let node_id = if has_easytier_id {
            self.easy_tier.lock().await.node_id().unwrap_or_default()
        } else {
            String::new()
        };

        let entry = PlayerProfileEntry {
            name: self.player_name.clone(),
            machine_id: self.machine_id.clone(),
            vendor: self.vendor.clone(),
            easytier_id: has_easytier_id.then_some(node_id),
            kind: None,
        };
        let body =
            serde_json::to_vec(&entry).map_err(|e| ScaffoldingError::Protocol(e.to_string()))?;
        self.tcp
            .lock()
            .await
            .send(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_ping".into(),
                body,
            })
            .await
            .map(|_| ())
    }

    /// 协议协商：发送我方协议列表，取与中心端共有的协议。
    async fn negotiate_protocols(&self) -> Result<(), ScaffoldingError> {
        let my: Vec<String> = STANDARD_PROTOCOLS
            .iter()
            .map(|s| s.to_string())
            .chain(self.custom_protocol_keys.iter().cloned())
            .collect();
        let body = my.join("\0").into_bytes();

        let response = self
            .tcp
            .lock()
            .await
            .send(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "protocols".into(),
                body,
            })
            .await?;
        if response.is_success() {
            let center_protocols: Vec<String> = String::from_utf8_lossy(&response.body)
                .split('\0')
                .map(|s| s.to_string())
                .collect();
            let negotiated = negotiate(&my, &center_protocols);
            *self.negotiated.write().await = negotiated;
            log::info!("协议协商完成: {}", my.join(", "));
        }
        Ok(())
    }

    /// 心跳 ping（`c:ping`）：返回中心端是否响应成功。
    pub async fn ping(&self) -> Result<bool, ScaffoldingError> {
        let response = self
            .tcp
            .lock()
            .await
            .send(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "ping".into(),
                body: vec![0x42],
            })
            .await?;
        Ok(response.is_success())
    }

    /// 获取 MC 服务器端口（`c:server_port`）。
    pub async fn get_server_port(&self) -> Result<u16, ScaffoldingError> {
        let response = self
            .tcp
            .lock()
            .await
            .send(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "server_port".into(),
                body: Vec::new(),
            })
            .await?;
        let status = response.status;
        if (32..64).contains(&status) {
            return Err(ScaffoldingError::Protocol(format!(
                "MC 服务器未启动 (状态码: {status})"
            )));
        }
        if !response.is_success() {
            return Err(ScaffoldingError::Protocol(format!(
                "获取服务器端口失败 (状态码: {status})"
            )));
        }
        if response.body.len() == 2 {
            Ok(u16::from_be_bytes([response.body[0], response.body[1]]))
        } else {
            Err(ScaffoldingError::Protocol(format!(
                "服务器端口响应长度非法: {} 字节",
                response.body.len()
            )))
        }
    }

    /// 建立 MC 端口可连接地址，返回 (host, port)。
    /// 直连模式：无需重启，直接返回虚拟 IP + MC 端口。
    /// 端口转发模式：重启 EasyTier 加入 MC 转发规则，返回 127.0.0.1 + 本地端口。
    pub async fn map_minecraft_port(
        &self,
        _ct: CancellationToken,
    ) -> Result<(String, u16), ScaffoldingError> {
        let has_room = {
            let cfg_ready = self.config.lock().await.is_some();
            let center_ready = self.center.lock().await.is_some();
            cfg_ready && center_ready
        };
        if !has_room {
            return Err(ScaffoldingError::CenterConnection(
                "尚未加入房间".into(),
            ));
        }

        {
            let host = self.minecraft_host.lock().await.clone();
            let port = *self.minecraft_port.lock().await;
            if let (Some(h), Some(p)) = (host, port) {
                return Ok((h, p));
            }
        }

        let mc_port = self.get_server_port().await?;
        let center = self
            .center
            .lock()
            .await
            .clone()
            .ok_or_else(|| ScaffoldingError::CenterConnection("尚未加入房间".into()))?;

        if !*self.use_port_forward.lock().await {
            // 直连模式：虚拟 IP 可路由，MC 端口也可直连，无需重启
            log::info!("直连模式，MC 地址: {}:{mc_port}", center.virtual_ip);
            *self.minecraft_host.lock().await = Some(center.virtual_ip.clone());
            *self.minecraft_port.lock().await = Some(mc_port);
            return Ok((center.virtual_ip, mc_port));
        }

        // 端口转发模式：动态添加 MC 转发（管理 RPC 热更新，不重启实例）。
        // 重启会重建虚拟网络：Center 连接断裂后需重新学习路由，重试窗口内
        // 重连失败（实测 BrokenPipe）；热更新保持 Center 连接不断。
        let local_mc_port = find_free_local_port().await;
        log::info!(
            "MC 端口转发 127.0.0.1:{local_mc_port} -> {}:{mc_port}",
            center.virtual_ip
        );
        let forward = format!(
            "tcp://127.0.0.1:{local_mc_port}/{}:{mc_port}",
            center.virtual_ip
        );
        self.easy_tier.lock().await.add_port_forward(&forward).await?;
        {
            let mut cfg = self.config.lock().await;
            if let Some(c) = cfg.as_mut() {
                c.port_forwards.push(forward);
            }
        }

        // 转发监听建立即视为成功（RPC 返回 Ok 表示 apply_port_forwards 已生效）；
        // 不能用 try_connect 验证——MC 转发目标是对端 MC 服务器，非 SCF 协议端点。
        *self.minecraft_host.lock().await = Some("127.0.0.1".into());
        *self.minecraft_port.lock().await = Some(local_mc_port);
        Ok(("127.0.0.1".to_string(), local_mc_port))
    }

    /// 获取玩家列表（`c:player_profiles_list`）。
    pub async fn get_player_list(&self) -> Result<Vec<PlayerInfo>, ScaffoldingError> {
        let response = self
            .tcp
            .lock()
            .await
            .send(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_profiles_list".into(),
                body: Vec::new(),
            })
            .await?;

        let value: serde_json::Value = serde_json::from_slice(&response.body)
            .map_err(|e| ScaffoldingError::Protocol(e.to_string()))?;
        let Some(items) = value.as_array() else {
            return Ok(Vec::new());
        };

        let mut players = Vec::with_capacity(items.len());
        for item in items {
            let Some(obj) = item.as_object() else {
                continue;
            };
            players.push(PlayerInfo {
                name: obj
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                machine_id: obj
                    .get("machine_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                vendor: obj
                    .get("vendor")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                easytier_id: obj
                    .get("easytier_id")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from),
                kind: if obj
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("HOST")
                {
                    PlayerKind::Host
                } else {
                    PlayerKind::Guest
                },
            });
        }
        Ok(players)
    }

    /// 向联机中心发送任意（含自定义扩展）协议请求，返回原始响应。
    pub async fn send_raw(
        &self,
        request: &ProtocolRequest,
    ) -> Result<ProtocolResponse, ScaffoldingError> {
        self.tcp.lock().await.send(request).await
    }

    /// 调用无入参的自定义协议，响应体按 JSON 反序列化为 `TResp`。
    pub async fn send_json<TResp: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<TResp, ScaffoldingError> {
        let (namespace, request_type) = split_key(key)?;
        let response = self
            .tcp
            .lock()
            .await
            .send(&ProtocolRequest {
                namespace,
                request_type,
                body: Vec::new(),
            })
            .await?;
        let status = response.status;
        if !response.is_success() {
            return Err(ScaffoldingError::Protocol(format!(
                "协议 {key} 返回错误状态: {status}"
            )));
        }
        serde_json::from_slice(&response.body)
            .map_err(|e| ScaffoldingError::Protocol(e.to_string()))
    }

    /// 调用带入参的自定义协议：payload 以 JSON 编码为请求体，响应体按 JSON 反序列化为 `TResp`。
    pub async fn send_json_req<TReq: Serialize, TResp: DeserializeOwned>(
        &self,
        key: &str,
        payload: &TReq,
    ) -> Result<TResp, ScaffoldingError> {
        let (namespace, request_type) = split_key(key)?;
        let body = serde_json::to_vec(payload)
            .map_err(|e| ScaffoldingError::Protocol(e.to_string()))?;
        let response = self
            .tcp
            .lock()
            .await
            .send(&ProtocolRequest {
                namespace,
                request_type,
                body,
            })
            .await?;
        let status = response.status;
        if !response.is_success() {
            return Err(ScaffoldingError::Protocol(format!(
                "协议 {key} 返回错误状态: {status}"
            )));
        }
        serde_json::from_slice(&response.body)
            .map_err(|e| ScaffoldingError::Protocol(e.to_string()))
    }

    /// 退出房间：停止心跳、断开连接并停止本实例启动的 EasyTier。
    pub async fn leave(&self) {
        self.stop_heartbeat().await;
        self.tcp.lock().await.disconnect();
        let _ = self.easy_tier.lock().await.stop().await;
        log::info!("已退出房间");
    }

    /// 启动心跳循环（5s 间隔）；回调内不调用 `self` 方法，全部使用克隆的 `Arc`，避免锁重入。
    async fn start_heartbeat(&self, ct: CancellationToken) {
        let et = self.easy_tier.clone();
        let tcp = self.tcp.clone();
        let negotiated = self.negotiated.clone();
        let player_name = self.player_name.clone();
        let machine_id = self.machine_id.clone();
        let vendor = self.vendor.clone();
        let tx = self.connection_lost_tx.clone();

        let tx_callback = tx.clone();
        let callback = Box::new(
            move || -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>> {
                let (et2, tcp2, neg2, pn, mid, vd, tx2) = (
                    et.clone(),
                    tcp.clone(),
                    negotiated.clone(),
                    player_name.clone(),
                    machine_id.clone(),
                    vendor.clone(),
                    tx_callback.clone(),
                );
                Box::pin(async move {
                    if !tcp2.lock().await.is_connected() {
                        let _ = tx2.send(true);
                        return Ok(());
                    }
                    let has_easytier_id = neg2
                        .read()
                        .await
                        .contains(&"c:player_easytier_id".to_string());
                    let node_id = if has_easytier_id {
                        et2.lock().await.node_id().unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let body = serde_json::to_vec(&serde_json::json!({
                        "name": pn,
                        "machine_id": mid,
                        "vendor": vd,
                        "easytier_id": has_easytier_id.then_some(node_id),
                        "kind": serde_json::Value::Null,
                    }))
                    .map_err(|_| ())?;
                    tcp2.lock()
                        .await
                        .send(&ProtocolRequest {
                            namespace: "c".into(),
                            request_type: "player_ping".into(),
                            body,
                        })
                        .await
                        .map_err(|_| ())?;
                    Ok(())
                })
            },
        );
        let tx_failed = tx.clone();
        let on_failed: Option<Box<dyn Fn() + Send>> =
            Some(Box::new(move || {
                let _ = tx_failed.send(true);
            }));

        let ct2 = ct.clone();
        let mut hb = HeartbeatService::new(callback, on_failed);
        tokio::spawn(async move { hb.run(HEARTBEAT_INTERVAL, ct2).await });
        *self.heartbeat_ct.lock().await = Some(ct);
    }

    /// 停止心跳循环（幂等）。
    async fn stop_heartbeat(&self) {
        let token = self.heartbeat_ct.lock().await.take();
        if let Some(ct) = token {
            ct.cancel();
        }
    }
}

/// 拆分协议键为命名空间与请求类型；不含 `:` 时返回错误。
fn split_key(key: &str) -> Result<(String, String), ScaffoldingError> {
    match key.find(':') {
        Some(colon) => Ok((key[..colon].to_string(), key[colon + 1..].to_string())),
        None => Err(ScaffoldingError::Protocol(format!(
            "无效的协议键格式: {key}"
        ))),
    }
}

/// 获取本地空闲端口：绑定 `127.0.0.1:0` 取端口后立即释放。
async fn find_free_local_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地空闲端口失败");
    let port = listener
        .local_addr()
        .expect("获取本地端口失败")
        .port();
    drop(listener);
    port
}
