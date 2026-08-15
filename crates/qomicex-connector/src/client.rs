//! 连接器库入口客户端（ScaffoldingClient）。
//! 对应 C# `Qomicex.Connector/ScaffoldingClient.cs`：
//! 统一的房间创建 / 加入 / 优雅关闭入口，管理本客户端创建的联机中心与访客端。
//!
//! 锁纪律：所有共享状态通过 `tokio::sync::Mutex` 保护，**一次只持一个锁**，
//! 用完即释放（语句结束 / 块作用域即 drop），禁止跨 `await` 持锁，避免死锁。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::center::scaffolding_center::{PlayerPingHandler, ScaffoldingCenter};
use crate::error::ScaffoldingError;
use crate::guest::scaffolding_guest::ScaffoldingGuest;
use crate::models::room_code::RoomCode;
use crate::protocols::ProtocolHandler;
use crate::relay::nodes;
use crate::relay::provider::RelayNodeProvider;
use crate::util::CancellationToken;

/// 中继节点列表缓存 TTL：解析失败/节点变更可自愈，避免一次性瞬时故障
/// 把残缺列表锁死到进程生命周期（实测：单次 http 节点解析失败被静默跳过
/// 并永久缓存，导致后续 join 一直用缺节点的列表）。
const RELAY_NODE_CACHE_TTL: Duration = Duration::from_secs(300);

/// 连接器库入口客户端。
pub struct ScaffoldingClient {
    /// 覆盖的中继节点列表（非空时优先使用，替代远程获取）。
    override_relay_nodes: Option<Vec<String>>,
    /// 追加的中继节点列表（始终附加在最终列表尾部）。
    additional_relay_nodes: Option<Vec<String>>,
    /// HTTP 请求 User-Agent（为空时使用默认值）。
    user_agent: Option<String>,
    /// 首选节点地区（未指定时自动检测系统地区）。
    preferred_region: Option<String>,
    /// 节点服务端点覆盖（默认 `RelayNodeProvider::ENDPOINT`）。
    relay_endpoint: Option<String>,
    /// 已解析的中继节点缓存（`(解析时间, 列表)`；TTL 内复用，过期/空列表重新解析）。
    cached_relay_nodes: tokio::sync::Mutex<Option<(Instant, Vec<String>)>>,
    /// 本客户端创建的联机中心（Host 房间）列表。
    managed_centers: tokio::sync::Mutex<Vec<Arc<ScaffoldingCenter>>>,
    /// 本客户端创建的访客端（Guest 连接）列表。
    managed_guests: tokio::sync::Mutex<Vec<Arc<ScaffoldingGuest>>>,
}

impl ScaffoldingClient {
    /// 创建客户端。
    ///
    /// - `override_relay_nodes`：覆盖中继节点；`Some` 时跳过远程获取。
    /// - `additional_relay_nodes`：追加节点，无论是否覆盖都会附加。
    /// - `user_agent`：节点服务 HTTP 请求 User-Agent。
    /// - `preferred_region`：首选节点地区。
    pub fn new(
        override_relay_nodes: Option<Vec<String>>,
        additional_relay_nodes: Option<Vec<String>>,
        user_agent: Option<String>,
        preferred_region: Option<String>,
    ) -> Self {
        Self {
            override_relay_nodes,
            additional_relay_nodes,
            user_agent,
            preferred_region,
            relay_endpoint: None,
            cached_relay_nodes: tokio::sync::Mutex::new(None),
            managed_centers: tokio::sync::Mutex::new(Vec::new()),
            managed_guests: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    /// 注入节点服务端点（覆盖默认 `RelayNodeProvider::ENDPOINT`；默认行为不变）。
    pub fn with_relay_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.relay_endpoint = Some(endpoint.into());
        self
    }

    /// 解析中继节点列表（TTL 缓存）：
    /// 缓存未过期直接返回；`override` 非空 → `resolve(override, additional)`；
    /// 否则远程获取 → `resolve(fetched, additional)`。
    /// 空列表不写缓存（下次调用重新解析）；过期后重新解析，失败仍返回旧缓存结果，
    /// 避免远端暂时不可用时列表被清空。
    async fn resolve_relay_nodes(&self) -> Vec<String> {
        let now = Instant::now();
        let cached = self.cached_relay_nodes.lock().await.clone();
        if let Some((cached_at, nodes)) = cached.as_ref() {
            if !nodes.is_empty() && now.duration_since(*cached_at) < RELAY_NODE_CACHE_TTL {
                return nodes.clone();
            }
        }

        let nodes = if let Some(override_nodes) = &self.override_relay_nodes {
            nodes::resolve(Some(override_nodes), self.additional_relay_nodes.as_deref())
        } else {
            let provider = RelayNodeProvider::new(self.user_agent.clone(), self.preferred_region.clone());
            let provider = match &self.relay_endpoint {
                Some(ep) => provider.with_endpoint(ep.clone()),
                None => provider,
            };
            let fetched = provider.fetch().await;
            nodes::resolve(Some(&fetched), self.additional_relay_nodes.as_deref())
        };

        if nodes.is_empty() {
            if let Some((_, stale)) = cached.as_ref() {
                if !stale.is_empty() {
                    log::warn!("中继节点重新解析结果为空，回退到上次缓存列表");
                    return stale.clone();
                }
            }
        } else {
            *self.cached_relay_nodes.lock().await = Some((now, nodes.clone()));
        }
        nodes
    }

    /// 创建房间（Host）：生成房间码 → 解析中继节点 → 启动联机中心。
    /// `custom_protocols` 为自定义扩展协议（如 `qml:game_info`），自动参与广告与协商
    /// （对应 C# `CreateRoomAsync` 的 `customProtocols` 参数）。
    /// `player_ping_handler` 为可选的 `c:player_ping` 处理钩子（拓展接口）：调用方可注入
    /// 裁决逻辑（如踢人黑名单/重连审核），`None` 时使用标准 SCF 行为（入列 + 通知）。
    /// 成功后返回联机中心句柄，并纳入本客户端的托管列表（`close_all` 统一关闭）。
    pub async fn create_room(
        &self,
        player_name: String,
        machine_id: String,
        vendor: String,
        minecraft_port: u16,
        ct: CancellationToken,
        custom_protocols: Vec<Arc<dyn ProtocolHandler>>,
        player_ping_handler: Option<PlayerPingHandler>,
    ) -> Result<Arc<ScaffoldingCenter>, ScaffoldingError> {
        let room_code = RoomCode::generate();
        log::info!("创建房间: 端口 {minecraft_port}");

        let relay_nodes = self.resolve_relay_nodes().await;
        let center = Arc::new(ScaffoldingCenter::new(
            room_code,
            player_name,
            machine_id,
            vendor,
            minecraft_port,
            Some(relay_nodes),
            custom_protocols,
        ));
        if let Some(handler) = player_ping_handler {
            center.set_player_ping_handler(handler).await;
        }
        center.start(ct).await?;

        self.managed_centers.lock().await.push(center.clone());
        log::info!("房间创建成功，房间码: {}", center.room_code().raw());
        Ok(center)
    }

    /// 加入房间（Guest）：解析房间码 → 解析中继节点 → 连接联机中心。
    /// 成功后返回访客端句柄，并纳入本客户端的托管列表（`close_all` 统一退出）。
    pub async fn join_room(
        &self,
        room_code_str: &str,
        player_name: String,
        machine_id: String,
        vendor: String,
        custom_protocol_keys: Vec<String>,
        ct: CancellationToken,
    ) -> Result<Arc<ScaffoldingGuest>, ScaffoldingError> {
        let code = RoomCode::parse(room_code_str)?;
        log::info!("加入房间: {}", code.raw());

        let relay_nodes = self.resolve_relay_nodes().await;
        let guest = Arc::new(ScaffoldingGuest::new(
            player_name,
            machine_id,
            vendor,
            custom_protocol_keys,
            Some(relay_nodes),
        ));
        if let Err(e) = guest.connect(&code, ct).await {
            // connect 失败时清理本次启动的 EasyTier 实例，避免残留实例累积
            // （同名节点干扰路由、占用 RPC 端口，导致后续 join discover 超时）。
            guest.leave().await;
            return Err(e);
        }

        self.managed_guests.lock().await.push(guest.clone());
        log::info!("成功加入房间");
        Ok(guest)
    }

    /// 优雅关闭本客户端创建的全部房间/连接（对齐 C# `CloseAsync`）：
    /// Host 关闭房间、Guest 退出房间，最后清空托管列表。
    pub async fn close_all(&self, ct: CancellationToken) {
        let centers = std::mem::take(&mut *self.managed_centers.lock().await);
        for center in centers {
            if let Err(e) = center.close(ct.clone()).await {
                log::error!("关闭房间失败: {e}");
            }
        }

        let guests = std::mem::take(&mut *self.managed_guests.lock().await);
        for guest in guests {
            guest.leave().await;
        }
    }
}
