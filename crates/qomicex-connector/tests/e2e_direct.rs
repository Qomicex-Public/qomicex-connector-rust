//! 端到端集成测试（默认跳过，手动触发）：
//! 本地三实例（relay + host + guest）直连拓扑，验证 SCF 完整链路：
//! 开房 → 发现中心 → 协议协商 → player_ping → ping → server_port → 玩家列表 → 清理。
//!
//! 运行：`cargo test -p qomicex-connector --test e2e_direct -- --ignored --nocapture`
//! 注意：本机 IP 需可路由（smoltcp 无 loopback）；若 IP 变化请修改 `LAN_IP`。

use std::sync::Arc;
use std::time::Duration;

use easytier_core::config::toml::{ConfigLoader as _, NetworkIdentity, TomlConfig};
use easytier_core::instance::manager::ConfigFileControl;
use easytier_core::instance::manager::InstanceManager;

use qomicex_connector::center::tcp_server::TcpServer;
use qomicex_connector::core::center_discovery::try_parse_center;
use qomicex_connector::core::protocol_negotiator::negotiate;
use qomicex_connector::guest::tcp_client::TcpClient;
use qomicex_connector::models::easy_tier_node::EasyTierNode;
use qomicex_connector::models::player::{PlayerInfo, PlayerKind};
use qomicex_connector::models::protocol::ProtocolRequest;
use qomicex_connector::protocols::{
    PingProtocol, PlayerPingProtocol, PlayerProfilesListProtocol, ProtocolHandler,
    ProtocolsProtocol, ServerPortProtocol,
};
use qomicex_connector::util::CancellationToken;

/// 本机局域网 IP（smoltcp 无 loopback，不能用 127.0.0.1）。
const LAN_IP: &str = "192.168.1.15";
const NETWORK_NAME: &str = "scaffolding-e2e-test";
const NETWORK_SECRET: &str = "e2e-secret";
const SCF_PORT: u16 = 1025;
const MC_PORT: u16 = 25565;
const HOST_ET_PORT: u16 = 11010;

type NativeMgr = InstanceManager<easytier::instance::factory::NativeInstanceFactory>;

async fn start_instance(
    hostname: &str,
    ipv4: Option<&str>,
    listeners: Vec<String>,
    peers: Vec<String>,
    whitelist: Vec<String>,
    port_forwards: Vec<String>,
) -> (uuid::Uuid, Arc<NativeMgr>) {
    let cfg = TomlConfig::default();
    cfg.set_network_identity(NetworkIdentity::new(NETWORK_NAME.to_string(), NETWORK_SECRET.to_string()));
    cfg.set_hostname(Some(hostname.to_string()));
    if let Some(ip) = ipv4 {
        cfg.set_ipv4(Some(format!("{ip}/24").parse().expect("ipv4 解析")));
    } else {
        cfg.set_dhcp(true);
    }
    cfg.set_listeners(listeners.iter().map(|l| l.parse().expect("listener URL")).collect());
    cfg.set_peers(peers.iter().map(|p| easytier_core::config::toml::PeerConfig {
        uri: p.parse().expect("peer URL"), peer_public_key: None,
    }).collect());
    cfg.set_tcp_whitelist(whitelist);
    cfg.set_port_forwards(port_forwards.iter().map(|pf| parse_port_forward(pf)).collect());
    let mut flags = cfg.get_flags();
    flags.no_tun = true;
    flags.use_smoltcp = true;
    flags.multi_thread = true;
    flags.latency_first = true;
    flags.data_compress_algo = easytier::proto::common::CompressionAlgoPb::Zstd.into();
    cfg.set_flags(flags);

    let manager = Arc::new(easytier::instance::factory::native_instance_manager());
    let id = manager.run_network_instance(cfg, ConfigFileControl::STATIC_CONFIG).expect("实例启动");
    for _ in 0..60 {
        if manager.instance(id).is_some_and(|i| i.is_ready()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    (id, manager)
}

/// 解析 `tcp://127.0.0.1:LOCAL/REMOTE:PORT` → PortForwardConfig（对应 C# 参数格式）。
fn parse_port_forward(s: &str) -> easytier_core::config::toml::PortForwardConfig {
    let rest = s.trim_start_matches("tcp://");
    let (bind, dst) = rest.split_once('/').expect("转发格式 tcp://bind/dst");
    easytier_core::config::toml::PortForwardConfig {
        bind_addr: bind.parse().expect("bind 地址"),
        dst_addr: dst.parse().expect("dst 地址"),
        proto: "tcp".to_string(),
    }
}

#[tokio::test]
#[ignore = "需要真实 EasyTier 实例与网络，手动触发"]
async fn e2e_direct_full_scf_flow() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    // ===== Host 侧：SCF TcpServer + 标准协议 =====
    // 玩家列表用 std Mutex（协议回调为同步闭包，不能在 async 上下文 blocking_read）
    let players: Arc<std::sync::Mutex<Vec<PlayerInfo>>> = Arc::new(std::sync::Mutex::new(vec![
        PlayerInfo { name: "HostSteve".into(), machine_id: "machine-host-0001".into(), easytier_id: Some("1".into()), vendor: "qml".into(), kind: PlayerKind::Host },
    ]));
    let players_ping = players.clone();
    let players_list = players.clone();
    let protocols: Vec<Arc<dyn ProtocolHandler>> = vec![
        Arc::new(PingProtocol),
        Arc::new(ProtocolsProtocol::new(vec![
            "c:ping".into(), "c:protocols".into(), "c:server_port".into(),
            "c:player_ping".into(), "c:player_profiles_list".into(), "c:player_easytier_id".into(),
        ])),
        Arc::new(ServerPortProtocol::new(MC_PORT)),
        Arc::new(PlayerPingProtocol::new(move |mut info| {
            let mut p = players_ping.lock().unwrap();
            if let Some(existing) = p.iter_mut().find(|x| x.machine_id == info.machine_id) {
                existing.name = info.name;
                existing.easytier_id = info.easytier_id;
                existing.vendor = info.vendor;
            } else {
                // 对应 ScaffoldingCenter::on_player_ping_impl：新玩家标记为 Guest
                info.kind = PlayerKind::Guest;
                p.push(info);
            }
            true
        })),
        Arc::new(PlayerProfilesListProtocol::new(move || players_list.lock().unwrap().clone())),
    ];

    let (disconnect_tx, _disconnect_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tcp_server = TcpServer::new(SCF_PORT, protocols, disconnect_tx);
    let srv_ct = CancellationToken::new();
    let srv_ct2 = srv_ct.clone();
    tokio::spawn(async move {
        let _ = tcp_server.start(srv_ct2).await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // ===== EasyTier：Host 固定 listener + 固定虚拟 IP =====
    let (host_id, host_mgr) = start_instance(
        &format!("scaffolding-mc-server-{SCF_PORT}"),
        Some("10.144.144.1"),
        vec![format!("tcp://{LAN_IP}:{HOST_ET_PORT}")],
        vec![],
        vec!["0".to_string(), SCF_PORT.to_string(), MC_PORT.to_string()],
        vec![],
    ).await;

    // ===== EasyTier：Guest DHCP + 直连 Host + 端口转发（127.0.0.1:LOCAL → 虚拟 IP:SCF 端口）=====
    // no_tun 模式宿主无法直连虚拟 IP，必须走 easytier 端口转发（对应 ScaffoldingGuest 转发模式）
    let local_scf_port: u16 = 10250;
    let (guest_id, guest_mgr) = start_instance(
        "scaffolding-mc-guest-e2etest01",
        None,
        vec!["tcp://0.0.0.0:0".to_string(), "udp://0.0.0.0:0".to_string()],
        vec![format!("tcp://{LAN_IP}:{HOST_ET_PORT}")],
        vec![],
        vec![format!("tcp://127.0.0.1:{local_scf_port}/10.144.144.1:{SCF_PORT}")],
    ).await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    // ===== Guest 发现中心 =====
    let nodes = guest_mgr.instance(guest_id).expect("guest 实例")
        .route_snapshots().await
        .into_iter()
        .map(|r| EasyTierNode {
            virtual_ip: r.ipv4_addr.and_then(|i| i.address).map(|a| a.to_string()).unwrap_or_default(),
            hostname: r.hostname.clone(),
            node_id: r.peer_id.to_string(),
        })
        .collect::<Vec<_>>();
    let center = try_parse_center(&nodes).expect("应发现联机中心");
    assert_eq!(center.virtual_ip, "10.144.144.1");
    assert_eq!(center.port, SCF_PORT);
    println!("[e2e] 发现中心: {}:{}", center.virtual_ip, center.port);

    // ===== 端口转发模式：连接 127.0.0.1:local_scf_port（easytier 转发到虚拟网络）=====
    let mut tcp = TcpClient::new();
    match tcp.connect("127.0.0.1", local_scf_port).await {
        Ok(()) => println!("[e2e] 端口转发连接成功"),
        Err(e) => panic!("连接中心（端口转发）失败: {e}"),
    }
    let my_protocols = ["c:ping", "c:protocols", "c:server_port", "c:player_ping", "c:player_profiles_list", "c:player_easytier_id"];
    let resp = tcp.send(&ProtocolRequest {
        namespace: "c".into(), request_type: "protocols".into(),
        body: my_protocols.join("\0").into_bytes(),
    }).await.expect("协商请求");
    assert!(resp.is_success());
    let center_protocols = String::from_utf8(resp.body).expect("UTF8").split('\0').map(String::from).collect::<Vec<_>>();
    let negotiated = negotiate(&my_protocols.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &center_protocols);
    assert!(negotiated.contains(&"c:ping".to_string()));
    println!("[e2e] 协商协议: {negotiated:?}");

    // ===== player_ping / ping / server_port =====
    let ping_body = serde_json::json!({"name":"GuestAlex","machine_id":"machine-guest-0002","vendor":"qml","easytier_id":null,"kind":null});
    let resp = tcp.send(&ProtocolRequest {
        namespace: "c".into(), request_type: "player_ping".into(), body: serde_json::to_vec(&ping_body).unwrap(),
    }).await.expect("player_ping");
    assert!(resp.is_success());
    println!("[e2e] player_ping 上报成功");

    let resp = tcp.send(&ProtocolRequest {
        namespace: "c".into(), request_type: "ping".into(), body: vec![0x42],
    }).await.expect("ping");
    assert!(resp.is_success() && resp.body == vec![0x42]);
    println!("[e2e] ping 回显成功");

    let resp = tcp.send(&ProtocolRequest {
        namespace: "c".into(), request_type: "server_port".into(), body: vec![],
    }).await.expect("server_port");
    let port = u16::from_be_bytes([resp.body[0], resp.body[1]]);
    assert_eq!(port, MC_PORT);
    println!("[e2e] server_port: {port}");

    // ===== 玩家列表（Host + Guest）=====
    let resp = tcp.send(&ProtocolRequest {
        namespace: "c".into(), request_type: "player_profiles_list".into(), body: vec![],
    }).await.expect("玩家列表");
    let list: Vec<serde_json::Value> = serde_json::from_slice(&resp.body).expect("JSON 数组");
    assert_eq!(list.len(), 2);
    assert!(list.iter().any(|p| p["kind"] == "HOST"));
    assert!(list.iter().any(|p| p["kind"] == "GUEST"));
    println!("[e2e] 玩家列表: {list:?}");

    // ===== 未知协议 → 255 =====
    let resp = tcp.send(&ProtocolRequest {
        namespace: "c".into(), request_type: "unknown_xyz".into(), body: vec![],
    }).await.expect("未知协议");
    assert_eq!(resp.status, 255);
    println!("[e2e] 未知协议返回 255");

    // ===== 清理 =====
    tcp.disconnect();
    srv_ct.cancel();
    guest_mgr.delete_network_instances([guest_id]).await.expect("guest 停止");
    host_mgr.delete_network_instances([host_id]).await.expect("host 停止");
    println!("[e2e] 全流程通过");
}
