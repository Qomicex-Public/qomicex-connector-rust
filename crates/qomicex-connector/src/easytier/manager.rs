//! EasyTier 管理器（easytier 库版，非进程）。
//! 对应 C# `EasyTierManager`（进程版）：不再 spawn `easytier-core`，直接在进程内运行网络实例。

use std::{net::SocketAddr, sync::Arc, time::Duration};

use easytier::instance::factory::{native_instance_manager, NativeInstanceFactory};
use easytier_core::{
    config::toml::{ConfigLoader as _, NetworkIdentity, PeerConfig, PortForwardConfig, TomlConfig},
    instance::manager::{ConfigFileControl, InstanceManager},
};
use easytier_proto::{common::CompressionAlgoPb, core_peer::peer::Route};
use log::{debug, info, warn};
use uuid::Uuid;

use crate::{
    error::ScaffoldingError,
    models::{easy_tier_node::EasyTierNode, network_config::NetworkConfig},
    relay,
};

/// 启动超时 30s / 轮询 500ms / 成功缓冲 2s（对齐 C# `WaitForStartupAsync`）。
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const STARTUP_BUFFER: Duration = Duration::from_secs(2);
/// 固定 IPv4 缺省掩码：C# 传纯 IP，easytier 需带掩码，故拼接 /24。
const FIXED_IPV4_PREFIX: u8 = 24;

/// EasyTier 管理器：管理单个 EasyTier 网络实例。
pub struct EasyTierManager {
    manager: Arc<InstanceManager<NativeInstanceFactory>>,
    /// 当前实例 ID（None = 未启动）。
    instance_id: Option<Uuid>,
    /// 虚拟 IP 缓存（Host 固定 ipv4 去掩码；Guest DHCP 取节点快照）。
    virtual_ip: Option<String>,
    /// 节点 ID 缓存（`instance.peer_id()`）。
    node_id: Option<String>,
}

impl EasyTierManager {
    /// 创建管理器（进程内运行，无需外部可执行文件）。
    pub fn new() -> Self {
        Self { manager: Arc::new(native_instance_manager()), instance_id: None, virtual_ip: None, node_id: None }
    }

    /// 是否正在运行（实例存在且就绪，对齐 C# `IsRunning`）。
    pub fn is_running(&self) -> bool {
        self.instance_id.and_then(|id| self.manager.instance(id)).is_some_and(|i| i.is_ready())
    }

    /// 虚拟 IP（缓存）。
    pub fn virtual_ip(&self) -> Option<String> {
        self.virtual_ip.clone()
    }

    /// 节点 ID（缓存）。
    pub fn node_id(&self) -> Option<String> {
        self.node_id.clone()
    }

    /// 启动 EasyTier 网络实例（对齐 C# `StartAsync` / `WaitForStartupAsync`）。
    pub async fn start(&mut self, config: &NetworkConfig) -> Result<(), ScaffoldingError> {
        if self.is_running() {
            return Err(ScaffoldingError::EasyTierStart("EasyTier 已在运行中".into()));
        }
        self.virtual_ip = None;
        self.node_id = None;

        let cfg = build_toml_config(config)?;
        let instance_id = self
            .manager
            .run_network_instance(cfg, ConfigFileControl::STATIC_CONFIG)
            .map_err(|err| ScaffoldingError::EasyTierStart(format!("启动 EasyTier 实例失败: {err}")))?;
        self.instance_id = Some(instance_id);
        info!("启动 EasyTier 实例: {instance_id}");

        // 轮询就绪（≤30s，500ms）。对齐 C#：超时抛异常但实例保持运行。
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        while !self.is_running() {
            if tokio::time::Instant::now() >= deadline {
                return Err(ScaffoldingError::EasyTierTimeout("EasyTier 启动超时 (30s)".into()));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        tokio::time::sleep(STARTUP_BUFFER).await;

        if let Some(instance) = self.manager.instance(instance_id) {
            let node_id = instance.peer_id().to_string();
            self.node_id = Some(node_id.clone());
            debug!("解析到节点 ID: {node_id}");
            if let Some(ip) = &config.ipv4 {
                self.virtual_ip = Some(strip_cidr(ip));
            } else if let Some(addr) = instance.node_snapshot().await.ipv4_addr {
                self.virtual_ip = Some(addr.address().to_string());
            }
            if let Some(ip) = &self.virtual_ip {
                debug!("解析到虚拟 IP: {ip}");
            }
        }
        Ok(())
    }

    /// 停止 EasyTier 网络实例（对齐 C# `StopAsync`）。
    pub async fn stop(&mut self) -> Result<(), ScaffoldingError> {
        if let Some(instance_id) = self.instance_id.take() {
            info!("停止 EasyTier...");
            // 对齐 C#：停止异常仅记录，不向上传播。
            if let Err(err) = self.manager.delete_network_instances([instance_id]).await {
                warn!("停止 EasyTier 时异常: {err}");
            }
        }
        self.virtual_ip = None;
        self.node_id = None;
        Ok(())
    }

    /// 获取网络中所有节点（对齐 C# `GetNodesAsync`）。
    pub async fn get_nodes(&self) -> Vec<EasyTierNode> {
        let Some(instance_id) = self.instance_id else {
            return Vec::new();
        };
        let Some(instance) = self.manager.instance(instance_id) else {
            return Vec::new();
        };
        instance
            .route_snapshots()
            .await
            .into_iter()
            .map(route_to_node)
            .collect()
    }

    /// 关闭与指定 easytier 节点（peer_id）的全部连接（踢人场景的物理断开）。
    ///
    /// 仅断开当前连接；对方若持续在线且无控制面 deny，easytier 可能自动重连
    /// （当前 fork 无 peer 级准入黑名单）。
    pub async fn disconnect_peer(&self, peer_id: &str) -> Result<(), ScaffoldingError> {
        let Some(instance_id) = self.instance_id else {
            return Ok(());
        };
        let Some(instance) = self.manager.instance(instance_id) else {
            return Ok(());
        };
        let peer_id: easytier_core::config::PeerId = peer_id
            .parse()
            .map_err(|e| ScaffoldingError::EasyTierStart(format!("无效的 peer id: {peer_id}: {e}")))?;
        instance
            .disconnect_peer(peer_id)
            .await
            .map_err(|e| ScaffoldingError::EasyTierStart(format!("断开 peer 失败: {e}")))
    }

    /// 运行中动态添加端口转发（管理 RPC 热更新，不重启实例）。
    ///
    /// 重启会重建虚拟网络：新实例需重连中继并重新学习路由，重试窗口内
    /// 转发目标不可达（实测 port-forward BrokenPipe）。热更新保持既有
    /// Center 连接不断，转发规则立即生效。
    pub async fn add_port_forward(&self, forward: &str) -> Result<(), ScaffoldingError> {
        let Some(instance_id) = self.instance_id else {
            return Ok(());
        };
        if self.manager.instance(instance_id).is_none() {
            return Ok(());
        }
        let (bind, dst, proto) = parse_forward_parts(forward)?;
        let payload = serde_json::json!({
            "patch": {
                "port_forwards": [{
                    "action": "ADD",
                    "cfg": {
                        "bind_addr": { "ipv4": { "addr": ipv4_to_u32(&bind.0) }, "port": bind.1 },
                        "dst_addr": { "ipv4": { "addr": ipv4_to_u32(&dst.0) }, "port": dst.1 },
                        "socket_type": if proto == "udp" { "UDP" } else { "TCP" }
                    }
                }]
            }
        });
        easytier_core::management::call_instance_json_rpc(
            &self.manager,
            "api.config.ConfigRpcService",
            "patch_config",
            None,
            payload,
        )
        .await
        .map(|_| ())
        .map_err(|e| ScaffoldingError::EasyTierStart(format!("动态添加端口转发失败: {forward}: {e}")))
    }
}

/// 构建 TomlConfig，与 C# `BuildArgs` 一一对应。
fn build_toml_config(config: &NetworkConfig) -> Result<TomlConfig, ScaffoldingError> {
    let cfg = TomlConfig::default();

    // --network-name / --network-secret / --hostname
    cfg.set_network_identity(NetworkIdentity::new(config.network_name.clone(), config.network_secret.clone()));
    cfg.set_hostname(Some(config.hostname.clone()));

    // --ipv4 / --dhcp（C# 先判 ipv4 再判 dhcp；纯 IP 时拼接 /24）
    if let Some(ipv4) = &config.ipv4 {
        let with_prefix = if ipv4.contains('/') { ipv4.clone() } else { format!("{ipv4}/{FIXED_IPV4_PREFIX}") };
        let addr: cidr::Ipv4Inet = with_prefix.parse().map_err(|_| ScaffoldingError::EasyTierStart(format!("无效的 ipv4 地址: {ipv4}")))?;
        cfg.set_ipv4(Some(addr));
    } else {
        cfg.set_dhcp(config.dhcp);
    }

    // -l tcp://0.0.0.0:0 -l udp://0.0.0.0:0（ListenRandomPorts；port 0 = 随机端口，与 C# 一致；vec![] 表示完全不监听（仅出站），故不用）
    // bind_ip 非空时监听绑定指定物理网卡 IP：出站 socket 复用 listener 的本地地址，
    // 源地址固定在物理网卡上，规避 VPN 虚拟网卡（Radmin 等）抢默认路由导致的单向劫持
    // （实测：出站 UDP 从 radmin 网卡发出但回包进不来 → 中继不可达 → 房间加入失败）。
    if config.listen_random_ports {
        let bind = config.bind_ip.as_deref().unwrap_or("0.0.0.0");
        let listeners = [format!("tcp://{bind}:0"), format!("udp://{bind}:0")]
            .into_iter()
            .map(|uri| uri.parse().map_err(|_| ScaffoldingError::EasyTierStart(format!("无效的监听地址: {uri}"))))
            .collect::<Result<Vec<_>, _>>()?;
        cfg.set_listeners(listeners);
    }

    // --compression=zstd --multi-thread --latency-first --enable-kcp-proxy
    let mut flags = cfg.get_flags();
    flags.no_tun = config.no_tun;
    flags.use_smoltcp = config.use_smoltcp;
    flags.multi_thread = true;
    flags.latency_first = true;
    flags.enable_kcp_proxy = true;
    flags.data_compress_algo = CompressionAlgoPb::Zstd.into();
    cfg.set_flags(flags);

    // --tcp-whitelist=0 --udp-whitelist=0（C# 先 --tcp-whitelist=0 再追加端口）
    let mut tcp_whitelist = vec!["0".to_string()];
    tcp_whitelist.extend(config.tcp_whitelist.iter().cloned());
    cfg.set_tcp_whitelist(tcp_whitelist);
    cfg.set_udp_whitelist(vec!["0".to_string()]);

    // -p {node}（C#: config.RelayNodes ?? RelayNodes.Default）
    let relay_nodes = config.relay_nodes.clone().unwrap_or_else(|| relay::nodes::resolve(None, None));
    let peers = relay_nodes
        .iter()
        .map(|node| node.parse().map(|uri| PeerConfig { uri, peer_public_key: None }).map_err(|_| ScaffoldingError::EasyTierStart(format!("无效的中继节点地址: {node}"))))
        .collect::<Result<Vec<_>, _>>()?;
    cfg.set_peers(peers);

    // --port-forward {pf}（格式: tcp://127.0.0.1:LOCAL/REMOTE:PORT）
    cfg.set_port_forwards(config.port_forwards.iter().map(|pf| parse_port_forward(pf)).collect::<Result<Vec<_>, _>>()?);

    Ok(cfg)
}

/// 解析端口转发规则：`tcp://127.0.0.1:LOCAL/REMOTE:PORT` → PortForwardConfig。
fn parse_port_forward(raw: &str) -> Result<PortForwardConfig, ScaffoldingError> {
    let invalid = |msg: String| ScaffoldingError::EasyTierStart(msg);
    let (proto, rest) = raw.split_once("://").ok_or_else(|| invalid(format!("无效的端口转发格式: {raw}")))?;
    let (bind, dst) = rest.split_once('/').ok_or_else(|| invalid(format!("无效的端口转发格式: {raw}")))?;
    let bind_addr: SocketAddr = bind.parse().map_err(|_| invalid(format!("无效的端口转发绑定地址: {bind}")))?;
    let dst_addr: SocketAddr = dst.parse().map_err(|_| invalid(format!("无效的端口转发目标地址: {dst}")))?;
    Ok(PortForwardConfig { bind_addr, dst_addr, proto: proto.to_string() })
}

/// Route → EasyTierNode（IP 去掩码，对齐 C# 对 `/24` 的截断）。
fn route_to_node(route: Route) -> EasyTierNode {
    let ip = route.ipv4_addr.and_then(|i| i.address).map(|a| std::net::Ipv4Addr::from(a).to_string()).unwrap_or_default();
    EasyTierNode { virtual_ip: ip, hostname: route.hostname, node_id: route.peer_id.to_string() }
}

/// 去除 IPv4 地址的掩码部分（对齐 C# 对 `/24` 的截断）。
fn strip_cidr(ip: &str) -> String {
    ip.split_once('/').map_or_else(|| ip.to_string(), |(addr, _)| addr.to_string())
}

/// 解析端口转发规则为 `(bind_addr, dst_addr, proto)`（仅支持 IPv4 地址）。
fn parse_forward_parts(
    raw: &str,
) -> Result<((String, u16), (String, u16), String), ScaffoldingError> {
    let invalid = |msg: String| ScaffoldingError::EasyTierStart(msg);
    let (proto, rest) = raw
        .split_once("://")
        .ok_or_else(|| invalid(format!("无效的端口转发格式: {raw}")))?;
    let (bind, dst) = rest
        .split_once('/')
        .ok_or_else(|| invalid(format!("无效的端口转发格式: {raw}")))?;
    let bind_addr: SocketAddr = bind
        .parse()
        .map_err(|_| invalid(format!("无效的端口转发绑定地址: {bind}")))?;
    let dst_addr: SocketAddr = dst
        .parse()
        .map_err(|_| invalid(format!("无效的端口转发目标地址: {dst}")))?;
    if !bind_addr.is_ipv4() || !dst_addr.is_ipv4() {
        return Err(invalid(format!("端口转发仅支持 IPv4 地址: {raw}")));
    }
    Ok((
        (bind_addr.ip().to_string(), bind_addr.port()),
        (dst_addr.ip().to_string(), dst_addr.port()),
        proto.to_string(),
    ))
}

/// IPv4 点分字符串 → 网络字节序 u32（对应 proto `Ipv4Addr.addr`）。
fn ipv4_to_u32(ip: &str) -> u32 {
    ip.split('.')
        .filter_map(|part| part.parse::<u32>().ok())
        .fold(0u32, |acc, octet| (acc << 8) | octet)
}
