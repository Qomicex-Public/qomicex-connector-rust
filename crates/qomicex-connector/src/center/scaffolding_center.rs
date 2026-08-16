//! 联机中心（ScaffoldingCenter）：管理 TCP 协议服务器、EasyTier 网络与玩家列表。
//! 对应 C# `Qomicex.Connector/Center/ScaffoldingCenter.cs`。
//!
//! 锁纪律：`players` 临界区均为同步短段（无 await）；同步上下文经
//! `try_read/try_write` + `yield_now` 重试等价 C# `lock(_playersLock)` 阻塞语义。

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use log::{error, info, warn};

use crate::center::tcp_server::TcpServer;
use crate::easytier::EasyTierManager;
use crate::error::ScaffoldingError;
use crate::models::easy_tier_node::EasyTierNode;
use crate::models::network_config::NetworkConfig;
use crate::models::player::{PlayerInfo, PlayerKind};
use crate::models::room_code::RoomCode;
use crate::protocols::{
    PingProtocol, PlayerPingProtocol, PlayerProfilesListProtocol, ProtocolsProtocol,
    ProtocolHandler, ServerPortProtocol,
};
use crate::util::CancellationToken;

/// 锁重试上限（临界区无 await，正常达不到；达到时放弃更新）。
const LOCK_RETRY_LIMIT: u32 = 10_000;
/// 玩家列表（`Arc` 供 `'static` 回调捕获，见翻译日志决策 1）。
type PlayerList = Arc<tokio::sync::RwLock<Vec<PlayerInfo>>>;
/// player_ping 处理钩子（拓展接口）：调用方可注入自定义裁决。
///
/// 返回 `true` → 响应状态 0（且 `handle_client` 刷新心跳）；返回 `false` → 状态 255
/// （不刷新心跳 → 15s 心跳超时兜底剔除）。入列与否由调用方闭包自行决定
/// （可调用 [`ScaffoldingCenter::handle_player_ping`] 委托标准 SCF 入列行为）。
/// 典型用途：房主侧踢人黑名单/重连审核（业务功能由调用方实现，库不内置）。
pub type PlayerPingHandler = Arc<dyn Fn(PlayerInfo) -> bool + Send + Sync>;

/// 联机中心。
pub struct ScaffoldingCenter {
    room_code: RoomCode,
    player_name: String,
    machine_id: String,
    vendor: String,
    minecraft_port: u16,
    easy_tier: Arc<tokio::sync::Mutex<EasyTierManager>>,
    /// 玩家列表（Arc 供 `'static` 回调捕获）。
    players: PlayerList,
    /// TCP 服务器（`None` = 未启动）。
    tcp_server: Arc<tokio::sync::Mutex<Option<TcpServer>>>,
    /// 玩家列表变更通知发送端（对应 C# `PlayersChanged` 事件）。
    players_changed_tx: tokio::sync::watch::Sender<Vec<PlayerInfo>>,
    relay_nodes: Option<Vec<String>>,
    /// 自定义扩展协议（对应 C# `_customProtocols`，参与广告与协商）。
    custom_protocols: Vec<Arc<dyn ProtocolHandler>>,
    /// 扫描选中的 TCP 端口（支撑同步 `tcp_port()`）。
    tcp_port: AtomicU16,
    /// 服务器任务取消令牌（供 `close()` 停止 accept 循环）。
    server_ct: tokio::sync::Mutex<Option<CancellationToken>>,
    /// player_ping 处理钩子（拓展接口，调用方注入；`None` = 标准 SCF 行为）。
    player_ping_handler: tokio::sync::RwLock<Option<PlayerPingHandler>>,
}

impl ScaffoldingCenter {
    /// 创建联机中心（对应 C# 构造函数；EasyTier 管理器由本类型内部创建）。
    pub fn new(
        room_code: RoomCode,
        player_name: String,
        machine_id: String,
        vendor: String,
        minecraft_port: u16,
        relay_nodes: Option<Vec<String>>,
        custom_protocols: Vec<Arc<dyn ProtocolHandler>>,
    ) -> Self {
        let (players_changed_tx, _) = tokio::sync::watch::channel(Vec::<PlayerInfo>::new());
        Self {
            room_code,
            player_name,
            machine_id,
            vendor,
            minecraft_port,
            easy_tier: Arc::new(tokio::sync::Mutex::new(EasyTierManager::new())),
            players: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            tcp_server: Arc::new(tokio::sync::Mutex::new(None)),
            players_changed_tx,
            relay_nodes,
            custom_protocols,
            tcp_port: AtomicU16::new(0),
            server_ct: tokio::sync::Mutex::new(None),
            player_ping_handler: tokio::sync::RwLock::new(None),
        }
    }

    /// 注入 player_ping 处理钩子（拓展接口）。须在 [`Self::start`] 之前调用才生效；
    /// 未注入时使用标准 SCF 行为（入列 + 通知）。
    pub async fn set_player_ping_handler(&self, handler: PlayerPingHandler) {
        *self.player_ping_handler.write().await = Some(handler);
    }

    /// 房间码。
    pub fn room_code(&self) -> &RoomCode { &self.room_code }
    /// 房主 MC 服务器端口。
    pub fn minecraft_port(&self) -> u16 { self.minecraft_port }
    /// 房主玩家名。
    pub fn player_name(&self) -> &str { &self.player_name }
    /// 房主机器标识。
    pub fn machine_id(&self) -> &str { &self.machine_id }
    /// 启动器厂商。
    pub fn vendor(&self) -> &str { &self.vendor }
    /// 玩家列表变更通知接收端（对应 C# `PlayersChanged` 事件）。
    pub fn players_changed_rx(&self) -> tokio::sync::watch::Receiver<Vec<PlayerInfo>> {
        self.players_changed_tx.subscribe()
    }
    /// 获取玩家列表快照（对应 C# `GetPlayers`）。
    pub fn get_players(&self) -> Vec<PlayerInfo> {
        with_players_read(&self.players, |l| l.to_vec()).unwrap_or_default()
    }
    /// 联机中心 TCP 端口（扫描选中的端口；未启动为 0）。
    pub fn tcp_port(&self) -> u16 { self.tcp_port.load(Ordering::Relaxed) }

    /// 启动联机中心（对应 C# `StartAsync`）：端口扫描 → TCP 服务器 → EasyTier → 房主入列。
    pub async fn start(&self, ct: CancellationToken) -> Result<(), ScaffoldingError> {
        // 端口扫描（对齐 C#：1025..65535 试绑即释放；全失败回退 25000）
        let mut tcp_port: u16 = 25000;
        for p in 1025u16..65535 {
            if tokio::net::TcpListener::bind(std::net::SocketAddr::from(([0, 0, 0, 0], p)))
                .await
                .is_ok()
            {
                tcp_port = p;
                break;
            }
        }
        self.tcp_port.store(tcp_port, Ordering::Relaxed);

        // 广告协议键 = 标准 6 键 + 自定义协议键（对应 C# `advertisedKeys.AddRange(...)`）
        const STANDARD_KEYS: [&str; 6] = ["c:ping", "c:protocols", "c:server_port", "c:player_ping", "c:player_profiles_list", "c:player_easytier_id"];
        // 重复键检查（对应 C# `ToDictionary` 遇重复键抛错；避免静默覆盖）
        let mut seen: Vec<&str> = STANDARD_KEYS.to_vec();
        for proto in &self.custom_protocols {
            let key = proto.key();
            if seen.contains(&key) {
                return Err(ScaffoldingError::Protocol(format!("自定义协议键冲突: {key}")));
            }
            seen.push(key);
        }
        let mut advertised_keys: Vec<String> = STANDARD_KEYS.into_iter().map(String::from).collect();
        advertised_keys.extend(self.custom_protocols.iter().map(|p| p.key().to_string()));

        // 断开事件通道（对齐 tcp_server 约定：空串映射回 None）
        let (disconnected_tx, mut disconnected_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        // 构建协议列表（对应 C# `new PlayerPingProtocol(OnPlayerPing)` 等 + `protocols.AddRange(_customProtocols)`）
        let players_ping = self.players.clone();
        let players_list = self.players.clone();
        let tx = self.players_changed_tx.clone();
        let player_ping_handler = self.player_ping_handler.read().await.clone();
        let mut protocols: Vec<Arc<dyn ProtocolHandler>> = vec![
            Arc::new(PingProtocol),
            Arc::new(ProtocolsProtocol::new(advertised_keys)),
            Arc::new(ServerPortProtocol::new(self.minecraft_port)),
            Arc::new(PlayerPingProtocol::new(move |info| {
                // 拓展点：调用方注入的 player_ping 处理钩子优先（裁决是否接受、是否入列，
                // 见 [`PlayerPingHandler`] 说明）；未注入 → 标准 SCF 行为（入列 + 通知）。
                if let Some(handler) = &player_ping_handler {
                    handler(info)
                } else {
                    on_player_ping_impl(&players_ping, &tx, info);
                    true
                }
            })),
            Arc::new(PlayerProfilesListProtocol::new(move || with_players_read(&players_list, |l| l.to_vec()).unwrap_or_default())),
        ];
        protocols.extend(self.custom_protocols.iter().cloned());

        // 消费客户端断开事件（对应 C# `ClientDisconnected += OnClientDisconnected`）
        let players = self.players.clone();
        let tx = self.players_changed_tx.clone();
        tokio::spawn(async move {
            while let Some(machine_id) = disconnected_rx.recv().await {
                on_client_disconnected_impl(&players, &tx, if machine_id.is_empty() { None } else { Some(machine_id) });
            }
        });

        // 创建并启动 TCP 服务器（对应 C# `Task.Run(StartAsync)` + `Task.Delay(200)`）
        *self.tcp_server.lock().await = Some(TcpServer::new(tcp_port, protocols, disconnected_tx));
        *self.server_ct.lock().await = Some(ct.clone());
        let server = self.tcp_server.clone();
        tokio::spawn(async move {
            let mut s = server.lock().await.take();
            let result = match s.as_mut() {
                Some(s) => s.start(ct).await,
                None => Ok(()),
            };
            if let Err(e) = result { error!("TcpServer 异常退出: {e}"); }
            if let Some(s) = s.as_mut() { s.stop(); } // 停止并放回（释放监听器）
            *server.lock().await = s;
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        info!("Scaffolding TCP 端口: {tcp_port}");

        let config = NetworkConfig {
            network_name: self.room_code.easy_tier_network_name(),
            network_secret: self.room_code.easy_tier_network_secret().to_string(),
            hostname: format!("scaffolding-mc-server-{tcp_port}"),
            // 管理员 → TUN 虚拟网卡（wintun）；非管理员保持默认 no-tun
            no_tun: !crate::util::is_elevated(),
            ipv4: Some("10.144.144.1".to_string()),
            dhcp: false,
            tcp_whitelist: vec![tcp_port.to_string(), self.minecraft_port.to_string()],
            relay_nodes: self.relay_nodes.clone(),
            bind_ip: crate::util::resolve_bind_ip(),
            ..Default::default()
        };
        self.easy_tier.lock().await.start(&config).await?;

        let node_id = self.easy_tier.lock().await.node_id();
        self.players.write().await.push(PlayerInfo { name: self.player_name.clone(), machine_id: self.machine_id.clone(), easytier_id: node_id, vendor: self.vendor.clone(), kind: PlayerKind::Host });
        notify_players_changed(&self.players, &self.players_changed_tx);
        info!("联机中心启动完成，房间码: {}", self.room_code.raw());
        Ok(())
    }

    /// 主动移除指定玩家（对应 C# `RemovePlayer`：房客优雅退出时的即时通知）。
    pub fn remove_player(&self, machine_id: &str) {
        on_client_disconnected_impl(&self.players, &self.players_changed_tx, Some(machine_id.to_string()));
    }

    /// 标准 SCF `c:player_ping` 行为：按 machine_id 更新既有玩家或作为 Guest 加入，并通知变更。
    /// 供调用方在自定义 player_ping 钩子（[`Self::set_player_ping_handler`]）中委托标准行为。
    pub fn handle_player_ping(&self, info: PlayerInfo) {
        on_player_ping_impl(&self.players, &self.players_changed_tx, info);
    }

    /// 按 machine_id 查询其 SCF TCP 连接源 IP（调用方反查 easytier peer 等用途）。
    pub async fn machine_source_ip(&self, machine_id: &str) -> Option<String> {
        let server = self.tcp_server.lock().await;
        match server.as_ref() {
            Some(s) => s.machine_source_ip(machine_id).await,
            None => None,
        }
    }

    /// 按 machine_id 定向断开其 SCF TCP 连接（调用方踢人/封禁等用途）。返回是否找到连接。
    pub async fn disconnect_machine(&self, machine_id: &str) -> bool {
        let server = self.tcp_server.lock().await;
        match server.as_ref() {
            Some(s) => s.disconnect_machine(machine_id).await,
            None => false,
        }
    }

    /// 当前 EasyTier 网络中全部节点快照（调用方按 hostname/虚拟 IP 反查 peer 等用途）。
    pub async fn easy_tier_nodes(&self) -> Vec<EasyTierNode> {
        self.easy_tier.lock().await.get_nodes().await
    }

    /// 断开与指定 easytier peer 的全部连接（调用方踢人/封禁等用途；对方在线可能自动重连）。
    pub async fn disconnect_peer(&self, peer_id: &str) -> Result<(), ScaffoldingError> {
        self.easy_tier.lock().await.disconnect_peer(peer_id).await
    }

    /// 将指定 easytier peer 加入 deny 黑名单并立即断开其全部连接（调用方踢人/封禁等用途；
    /// **持久**物理封禁——对方重连请求在连接建立处被拒，与 [`Self::disconnect_peer`] 的
    /// 瞬时断开不同）。
    pub async fn deny_peer(&self, peer_id: &str) -> Result<(), ScaffoldingError> {
        self.easy_tier.lock().await.deny_peer(peer_id).await
    }

    /// 解除 deny（允许该 easytier peer 重新连接）。
    pub async fn allow_peer(&self, peer_id: &str) -> Result<(), ScaffoldingError> {
        self.easy_tier.lock().await.allow_peer(peer_id).await
    }

    /// 关闭房间：停止 TCP 服务器并清理本实例启动的 EasyTier 实例（对应 C# `CloseAsync`）。
    pub async fn close(&self, _ct: CancellationToken) -> Result<(), ScaffoldingError> {
        if let Some(mut server) = self.tcp_server.lock().await.take() { server.stop(); } // C# `_tcpServer?.Stop()`
        if let Some(ct) = self.server_ct.lock().await.take() { ct.cancel(); } // accept 循环退出
        self.easy_tier.lock().await.stop().await?;
        info!("联机中心已关闭，房间码: {}", self.room_code.raw());
        Ok(())
    }
}

/// 玩家心跳处理（对应 C# `OnPlayerPing`）：按 machine_id 更新既有玩家，否则作为 Guest 加入。
fn on_player_ping_impl(players: &PlayerList, tx: &tokio::sync::watch::Sender<Vec<PlayerInfo>>, mut info: PlayerInfo) {
    with_players_write(players, |list| match list.iter_mut().find(|p| p.machine_id == info.machine_id) {
        Some(existing) => {
            existing.name = info.name;
            // 保守覆盖：仅当本次 ping 带有效 easytier_id 时才更新；null/缺失不覆盖已有 id
            // （第三方 guest 偶发 null ping 会清掉 id → 踢人时无法按 id deny_peer，只能反查兜底）。
            if info.easytier_id.is_some() {
                existing.easytier_id = info.easytier_id;
            }
            existing.vendor = info.vendor;
        }
        None => {
            info.kind = PlayerKind::Guest;
            info!("新玩家加入: {} ({})", info.name, info.machine_id);
            list.push(info);
        }
    });
    notify_players_changed(players, tx);
}

/// 客户端断开处理（对应 C# `OnClientDisconnected`）：Host 不剔除，其余按 machine_id 移除。
fn on_client_disconnected_impl(players: &PlayerList, tx: &tokio::sync::watch::Sender<Vec<PlayerInfo>>, machine_id: Option<String>) {
    if let Some(mid) = machine_id {
        with_players_write(players, |list| {
            if let Some(idx) = list.iter().position(|p| p.machine_id == mid) {
                if list[idx].kind != PlayerKind::Host {
                    let removed = list.remove(idx);
                    info!("玩家已离开: {} ({})", removed.name, removed.machine_id);
                }
            }
        });
    }
    notify_players_changed(players, tx);
}

/// 玩家列表变更通知（对应 C# `NotifyPlayersChanged`）。
fn notify_players_changed(players: &PlayerList, tx: &tokio::sync::watch::Sender<Vec<PlayerInfo>>) {
    let _ = tx.send(with_players_read(players, |l| l.to_vec()).unwrap_or_default());
}

/// 写锁重试（对齐 C# `lock(_playersLock)` 阻塞语义；临界区无 await）。
fn with_players_write(players: &PlayerList, f: impl FnOnce(&mut Vec<PlayerInfo>)) {
    for _ in 0..LOCK_RETRY_LIMIT {
        if let Ok(mut guard) = players.try_write() {
            f(&mut guard);
            return;
        }
        std::thread::yield_now();
    }
    warn!("玩家列表写锁等待超时，更新已跳过");
}

/// 读锁重试（对齐 C# `lock(_playersLock)` 阻塞语义；临界区无 await）。
fn with_players_read<R>(players: &PlayerList, f: impl FnOnce(&[PlayerInfo]) -> R) -> Option<R> {
    for _ in 0..LOCK_RETRY_LIMIT {
        if let Ok(guard) = players.try_read() {
            return Some(f(&guard));
        }
        std::thread::yield_now();
    }
    warn!("玩家列表读锁等待超时");
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::protocol::ProtocolRequest;
    use crate::protocols::PlayerPingProtocol;
    use serde_json::json;

    fn ping_body(machine_id: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "name": "KickedPlayer",
            "machine_id": machine_id,
            "vendor": "third-party"
        }))
        .expect("构造测试体失败")
    }

    fn guest_info(machine_id: &str) -> PlayerInfo {
        PlayerInfo {
            name: "Guest".into(),
            machine_id: machine_id.into(),
            vendor: "third-party".into(),
            easytier_id: None,
            kind: PlayerKind::Guest,
        }
    }

    fn new_center() -> ScaffoldingCenter {
        ScaffoldingCenter::new(
            RoomCode::generate(),
            "Host".into(),
            "h1".into(),
            "qml".into(),
            25565,
            None,
            vec![],
        )
    }

    #[tokio::test]
    async fn handle_player_ping_adds_guest_and_updates() {
        let center = new_center();
        // 标准 SCF 行为：新玩家 ping → 作为 Guest 入列
        center.handle_player_ping(guest_info("g1"));
        let players = center.get_players();
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].machine_id, "g1");
        assert_eq!(players[0].kind, PlayerKind::Guest);

        // 再次 ping（更新字段）→ 不重复入列
        center.handle_player_ping(PlayerInfo {
            name: "Renamed".into(),
            machine_id: "g1".into(),
            vendor: "qml".into(),
            easytier_id: Some("peer-1".into()),
            kind: PlayerKind::Guest,
        });
        let players = center.get_players();
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].name, "Renamed");
        assert_eq!(players[0].easytier_id.as_deref(), Some("peer-1"));

        // 保守覆盖：null/缺失 easytier_id 不得清掉已上报的 id（第三方偶发 null ping）
        center.handle_player_ping(PlayerInfo {
            name: "Renamed".into(),
            machine_id: "g1".into(),
            vendor: "qml".into(),
            easytier_id: None,
            kind: PlayerKind::Guest,
        });
        let players = center.get_players();
        assert_eq!(players[0].easytier_id.as_deref(), Some("peer-1"), "null ping 不应清掉已有 id");
    }

    #[tokio::test]
    async fn player_ping_handler_injection_is_stored_and_protocol_rejects() {
        let center = new_center();
        assert!(center.player_ping_handler.read().await.is_none());
        center.set_player_ping_handler(Arc::new(|_| false)).await;
        assert!(center.player_ping_handler.read().await.is_some());

        // 注入的裁决语义由调用方闭包实现（业务功能在调用方）；
        // 协议层契约：回调返回 false → 状态 255（不刷新心跳）。
        let handler = PlayerPingProtocol::new(|_| false);
        let response = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_ping".into(),
                body: ping_body("k1"),
            })
            .await;
        assert_eq!(response.status, 255);
    }
}
