//! 联机中心（ScaffoldingCenter）：管理 TCP 协议服务器、EasyTier 网络与玩家列表。
//! 对应 C# `Qomicex.Connector/Center/ScaffoldingCenter.cs`。
//!
//! 锁纪律：`players` 临界区均为同步短段（无 await）；同步上下文经
//! `try_read/try_write` + `yield_now` 重试等价 C# `lock(_playersLock)` 阻塞语义。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use log::{debug, error, info, warn};

use crate::center::tcp_server::TcpServer;
use crate::easytier::EasyTierManager;
use crate::error::ScaffoldingError;
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
/// 已踢玩家黑名单（machine_id → 踢出时解析到的 easytier peer id）。
type KickedSet = Arc<tokio::sync::RwLock<HashMap<String, KickedInfo>>>;

/// 房主对已踢玩家重连请求的审核动作（弹窗三选）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KickReviewAction {
    /// 允许重新加入：从黑名单移除，下一次 player_ping 正常入列。
    Allow,
    /// 拒绝：维持踢出，下次重连可再次询问。
    Reject,
    /// 拒绝且不再提示：后续重连静默拒绝，不再弹窗。
    RejectSilent,
}

/// 待房主审核的重连请求（`/connector/status` 暴露给前端弹窗）。
#[derive(Debug, Clone, Default)]
pub struct KickReview {
    /// 申请重连的玩家机器标识。
    pub machine_id: String,
    /// 玩家名。
    pub name: String,
    /// 启动器厂商。
    pub vendor: String,
}

/// 已踢玩家记录：保留踢出时解析到的 easytier peer id，供再次 ping 时重复断开。
#[derive(Debug, Clone, Default)]
struct KickedInfo {
    /// 踢出时解析到的 easytier peer（节点）id；`None` = 无法定位网络层（第三方 guest）。
    easytier_peer: Option<String>,
    /// 最近一次重连请求的玩家名（弹窗展示）。
    name: String,
    /// 最近一次重连请求的厂商（弹窗展示）。
    vendor: String,
    /// 重连审核中（弹窗已弹出，等待房主决定）。
    pending: bool,
    /// 拒绝且不再提示：后续重连静默拒绝，不弹窗。
    prompt_disabled: bool,
}

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
    /// 已踢玩家黑名单（防被踢 guest 经 player_ping 重新入列；房间关闭随实例释放）。
    kicked: KickedSet,
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
            kicked: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
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
        let kicked = self.kicked.clone();
        let tcp_server = self.tcp_server.clone();
        let easy_tier = self.easy_tier.clone();
        let mut protocols: Vec<Arc<dyn ProtocolHandler>> = vec![
            Arc::new(PingProtocol),
            Arc::new(ProtocolsProtocol::new(advertised_keys)),
            Arc::new(ServerPortProtocol::new(self.minecraft_port)),
            Arc::new(PlayerPingProtocol::new(move |info| {
                // 已踢玩家：进入重连审核状态机。
                if is_kicked(&kicked, &info.machine_id) {
                    let machine_id = info.machine_id.clone();
                    // ① 拒绝且不再提示 → 静默拒绝（255 + 断开 SCF TCP + easytier），不弹窗
                    if kicked_read(&kicked, &machine_id)
                        .map(|k| k.prompt_disabled)
                        .unwrap_or(false)
                    {
                        let peer = kicked_read(&kicked, &machine_id)
                            .and_then(|k| k.easytier_peer.clone());
                        let tcp_server = tcp_server.clone();
                        let easy_tier = easy_tier.clone();
                        tokio::spawn(async move {
                            if let Some(server) = tcp_server.lock().await.as_ref() {
                                if !server.disconnect_machine(&machine_id).await {
                                    debug!("已踢玩家 {machine_id} 的 SCF TCP 连接未找到（可能已断开）");
                                }
                            }
                            if let Some(peer) = peer {
                                if let Err(e) = easy_tier.lock().await.disconnect_peer(&peer).await {
                                    warn!("已踢玩家 {machine_id} 再次断开 easytier 失败: {e}");
                                }
                            }
                        });
                        return false;
                    }
                    // ② 首次重连请求：置 pending（前端轮询 status 弹窗询问房主），保持 SCF TCP
                    //    连接（响应 0 刷新心跳）；重复 ping 不再重复弹窗。easytier 持续断开（数据面封禁）。
                    if !kicked_read(&kicked, &machine_id)
                        .map(|k| k.pending)
                        .unwrap_or(false)
                    {
                        mark_kick_pending(&kicked, &machine_id, &info.name, &info.vendor);
                        info!("玩家 {machine_id} 申请重新加入，等待房主决定");
                    }
                    let peer = kicked_read(&kicked, &machine_id)
                        .and_then(|k| k.easytier_peer.clone());
                    let easy_tier = easy_tier.clone();
                    tokio::spawn(async move {
                        if let Some(peer) = peer {
                            if let Err(e) = easy_tier.lock().await.disconnect_peer(&peer).await {
                                warn!("已踢玩家 {machine_id} 等待审核期间断开 easytier 失败: {e}");
                            }
                        }
                    });
                    return true; // 0：保持连接等待决定（不刷新入列逻辑，player 不入列）
                }
                on_player_ping_impl(&players_ping, &tx, info);
                true
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

    /// 踢出玩家（房主手动断开指定 guest）：
    /// ① 解析其 easytier peer 并物理断开（优先已上报 easytier_id，否则 hostname / SCF 源虚拟 IP
    ///    反查；非 QML SCF 客户端不受 Scaffolding 协议控制，只能物理断开虚拟网络）；
    /// ② 记入已踢黑名单（后续 re-ping 进入重连审核，见 [`Self::pending_kick_reviews`]）；
    /// ③ 断开其 Scaffolding TCP（QML guest 心跳失败后自动整体退出）；④ 从玩家列表移除。
    pub async fn kick_player(&self, machine_id: &str) {
        // ① 解析 easytier peer id 并物理断开
        let player = self
            .players
            .read()
            .await
            .iter()
            .find(|p| p.machine_id == machine_id)
            .cloned();
        let peer_id = match player.as_ref().and_then(|p| p.easytier_id.clone()) {
            Some(id) => Some(id),
            None => self.resolve_guest_easytier_peer(machine_id).await,
        };
        if let Some(peer_id) = &peer_id {
            if let Err(e) = self.easy_tier.lock().await.disconnect_peer(peer_id).await {
                warn!("踢出玩家 {machine_id} 时断开 easytier 连接失败: {e}");
            }
        } else {
            warn!(
                "踢出玩家 {machine_id} 无法解析其 easytier peer（未上报 easytier_id 且 hostname/源IP 反查失败），仅断开 Scaffolding TCP + 拉黑"
            );
        }
        // ② 已踢黑名单（防 re-ping 回归；peer id 供再次 ping 时重复断开）
        self.kicked.write().await.insert(
            machine_id.to_string(),
            KickedInfo {
                easytier_peer: peer_id,
                name: player.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
                vendor: player.as_ref().map(|p| p.vendor.clone()).unwrap_or_default(),
                pending: false,
                prompt_disabled: false,
            },
        );
        // ③ 断开 Scaffolding TCP（存在则触发断开事件）
        if let Some(server) = self.tcp_server.lock().await.as_ref() {
            if !server.disconnect_machine(machine_id).await {
                warn!("踢出玩家 {machine_id} 时未找到其 Scaffolding TCP 连接（可能已断开）");
            }
        }
        // ④ 玩家列表移除
        self.remove_player(machine_id);
        info!("已踢出玩家: {machine_id}");
    }

    /// 待房主审核的重连请求列表（`pending` 标记的已踢玩家；供 `/connector/status` 暴露给前端弹窗）。
    pub async fn pending_kick_reviews(&self) -> Vec<KickReview> {
        let guard = self.kicked.read().await;
        guard
            .iter()
            .filter(|(_, k)| k.pending)
            .map(|(mid, k)| KickReview {
                machine_id: mid.clone(),
                name: k.name.clone(),
                vendor: k.vendor.clone(),
            })
            .collect()
    }

    /// 处理房主对重连请求的决定（弹窗三选）。
    ///
    /// - [`KickReviewAction::Allow`]：从黑名单移除，下一次 player_ping 正常入列。
    /// - [`KickReviewAction::Reject`]：维持踢出（pending 复位），断开其等待中的连接。
    /// - [`KickReviewAction::RejectSilent`]：同上，并置 `prompt_disabled`（后续重连不再弹窗）。
    pub async fn decide_kick_review(&self, machine_id: &str, action: KickReviewAction) {
        match action {
            KickReviewAction::Allow => {
                self.kicked.write().await.remove(machine_id);
                info!("房主允许玩家 {machine_id} 重新加入");
            }
            KickReviewAction::Reject => {
                if let Some(k) = self.kicked.write().await.get_mut(machine_id) {
                    k.pending = false;
                }
                self.drop_kicked_connection(machine_id).await;
                info!("房主拒绝玩家 {machine_id} 重新加入");
            }
            KickReviewAction::RejectSilent => {
                if let Some(k) = self.kicked.write().await.get_mut(machine_id) {
                    k.pending = false;
                    k.prompt_disabled = true;
                }
                self.drop_kicked_connection(machine_id).await;
                info!("房主拒绝玩家 {machine_id} 重新加入（不再提示）");
            }
        }
    }

    /// 断开已踢玩家的 SCF TCP 与 easytier（拒绝决定后的收尾）。
    async fn drop_kicked_connection(&self, machine_id: &str) {
        if let Some(server) = self.tcp_server.lock().await.as_ref() {
            if !server.disconnect_machine(machine_id).await {
                debug!("玩家 {machine_id} 的 SCF TCP 连接未找到（可能已断开）");
            }
        }
        let peer = self
            .kicked
            .read()
            .await
            .get(machine_id)
            .and_then(|k| k.easytier_peer.clone());
        if let Some(peer) = peer {
            if let Err(e) = self.easy_tier.lock().await.disconnect_peer(&peer).await {
                warn!("拒绝玩家 {machine_id} 时断开 easytier 失败: {e}");
            }
        }
    }

    /// 解析 guest 的 easytier peer id（未上报 easytier_id 时的兜底反查）：
    /// ① 按 hostname `scaffolding-mc-guest-{machine_id 前 8 字符}` 匹配（Qomicex 系 guest 约定，
    ///    对齐 Rust/C# guest 的 easytier hostname 命名）；② 按 SCF TCP 源虚拟 IP 匹配
    ///    （guest 的 SCF 连接走 easytier 虚拟网时源地址即其虚拟 IP，对第三方 guest 也有效）。
    /// 均失败返回 `None`（第三方 guest 无法定位网络层）。
    async fn resolve_guest_easytier_peer(&self, machine_id: &str) -> Option<String> {
        let nodes = self.easy_tier.lock().await.get_nodes().await;
        let hostname = format!(
            "scaffolding-mc-guest-{}",
            machine_id.chars().take(8).collect::<String>()
        );
        if let Some(node) = nodes.iter().find(|n| n.hostname == hostname) {
            info!("踢出 {machine_id}: 按 easytier hostname 反查命中 peer {}", node.node_id);
            return Some(node.node_id.clone());
        }
        if let Some(server) = self.tcp_server.lock().await.as_ref() {
            if let Some(src_ip) = server.machine_source_ip(machine_id).await {
                if let Some(node) = nodes.iter().find(|n| n.virtual_ip == src_ip) {
                    info!("踢出 {machine_id}: 按 SCF 源虚拟 IP {src_ip} 反查命中 peer {}", node.node_id);
                    return Some(node.node_id.clone());
                }
            }
        }
        None
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
            existing.easytier_id = info.easytier_id;
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

/// 已踢检查（同步回调内使用）：machine_id 在已踢黑名单中返回 `true`。
fn is_kicked(kicked: &KickedSet, machine_id: &str) -> bool {
    kicked_read(kicked, machine_id).is_some()
}

/// 已踢名单读取（同步回调内使用；读锁重试对齐 `with_players_read`）。
fn kicked_read(kicked: &KickedSet, machine_id: &str) -> Option<KickedInfo> {
    for _ in 0..LOCK_RETRY_LIMIT {
        if let Ok(guard) = kicked.try_read() {
            return guard.get(machine_id).cloned();
        }
        std::thread::yield_now();
    }
    warn!("已踢名单读锁等待超时");
    None
}

/// 置为待审核（同步回调内使用；写锁重试对齐 `with_players_write`）。
fn mark_kick_pending(kicked: &KickedSet, machine_id: &str, name: &str, vendor: &str) {
    for _ in 0..LOCK_RETRY_LIMIT {
        if let Ok(mut guard) = kicked.try_write() {
            if let Some(info) = guard.get_mut(machine_id) {
                info.pending = true;
                info.name = name.to_string();
                info.vendor = vendor.to_string();
            }
            return;
        }
        std::thread::yield_now();
    }
    warn!("已踢名单写锁等待超时，pending 更新已跳过");
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

    #[tokio::test]
    async fn kicked_guest_ping_is_rejected_not_readded() {
        let kicked: KickedSet = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        // 踢出场景：guest 未上报 easytier_id 且反查失败 → KickedInfo.easytier_peer = None
        kicked
            .write()
            .await
            .insert("k1".into(), KickedInfo { easytier_peer: None, ..Default::default() });

        // 黑名单生效：被踢 machine_id 判定为已踢，未踢的不受影响
        assert!(is_kicked(&kicked, "k1"));
        assert!(!is_kicked(&kicked, "k2"));

        // 协议层：被踢 guest 再发 c:player_ping → 拒绝（状态 255，不刷新心跳 → 15s 兜底剔除）
        let handler =
            PlayerPingProtocol::new(move |info| !is_kicked(&kicked, &info.machine_id));
        let rejected = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_ping".into(),
                body: ping_body("k1"),
            })
            .await;
        assert_eq!(rejected.status, 255);

        // 普通玩家 ping 正常接受
        let accepted = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_ping".into(),
                body: ping_body("k2"),
            })
            .await;
        assert_eq!(accepted.status, 0);
    }

    #[tokio::test]
    async fn kick_review_state_machine() {
        let kicked: KickedSet = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        kicked
            .write()
            .await
            .insert("k1".into(), KickedInfo { easytier_peer: None, ..Default::default() });

        // 重连请求 → pending（弹窗），名字/厂商记录
        mark_kick_pending(&kicked, "k1", "Alex", "third-party");
        let reviews = kicked.read().await;
        assert!(reviews.get("k1").unwrap().pending);
        assert_eq!(reviews.get("k1").unwrap().name, "Alex");
        assert_eq!(reviews.get("k1").unwrap().vendor, "third-party");
        drop(reviews);

        // 允许 → 移除黑名单
        {
            let mut g = kicked.write().await;
            g.remove("k1");
        }
        assert!(!is_kicked(&kicked, "k1"));

        // 拒绝且不再提示 → prompt_disabled
        kicked
            .write()
            .await
            .insert("k2".into(), KickedInfo { easytier_peer: None, ..Default::default() });
        mark_kick_pending(&kicked, "k2", "Bob", "third-party");
        {
            let mut g = kicked.write().await;
            if let Some(k) = g.get_mut("k2") {
                k.pending = false;
                k.prompt_disabled = true;
            }
        }
        let k2 = kicked.read().await.get("k2").cloned().unwrap();
        assert!(k2.prompt_disabled);
        assert!(!k2.pending);
    }
}
