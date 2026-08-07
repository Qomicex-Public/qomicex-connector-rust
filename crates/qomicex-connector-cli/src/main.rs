use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use easytier_core::config::toml::{ConfigLoader as _, NetworkIdentity, TomlConfig};
use easytier_core::instance::manager::ConfigFileControl;

use qomicex_connector::client::ScaffoldingClient;
use qomicex_connector::error::ScaffoldingError;
use qomicex_connector::util::CancellationToken;

#[derive(Parser)]
#[command(name = "qomicex-connector-cli")]
struct Cli {
    /// 中继节点（可重复指定，如 tcp://1.2.3.4:11010；不指定则在线获取）
    #[arg(long, value_name = "URL")]
    relay: Vec<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 本地中继节点（供测试/自建中继）
    #[command(alias = "relay")]
    Relay {
        /// 监听地址，如 0.0.0.0:11010（默认 0.0.0.0:11010）
        #[arg(long, default_value = "0.0.0.0:11010")]
        listen: String,
        /// 运行时长（秒），0 表示直到 Ctrl+C
        #[arg(long, default_value = "0")]
        seconds: u64,
    },
    /// 房主：创建房间
    #[command(alias = "create")]
    Host {
        mc_port: String,
        #[arg(default_value = "Qomicex-Player")]
        player_name: String,
    },
    /// 房客：加入房间
    #[command(alias = "join")]
    Guest {
        room_code: String,
        #[arg(default_value = "Qomicex-Player")]
        player_name: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    let relay = if cli.relay.is_empty() { None } else { Some(cli.relay) };

    match cli.command {
        Command::Relay { listen, seconds } => run_relay(&listen, seconds).await,
        _ => {
            let machine_id = uuid::Uuid::new_v4().simple().to_string();
            let machine_id = &machine_id[..12];
            let client = ScaffoldingClient::new(relay, None, None, None);
            let ct = CancellationToken::new();

            let result = match cli.command {
                Command::Host { mc_port, player_name } => {
                    let port: u16 = mc_port
                        .parse()
                        .map_err(|_| ScaffoldingError::Protocol("无效端口".into()))?;
                    run_host(&client, &ct, &player_name, machine_id, port).await
                }
                Command::Guest { room_code, player_name } => {
                    run_guest(&client, &ct, &room_code, &player_name, machine_id).await
                }
                _ => unreachable!(),
            };

            client.close_all(ct.clone()).await;

            match result {
                Ok(()) => Ok(()),
                Err(e) => {
                    println!("错误: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

async fn run_relay(listen: &str, seconds: u64) -> Result<(), Box<dyn Error>> {
    println!("启动本地中继，监听 {listen}...");
    let cfg = TomlConfig::default();
    cfg.set_network_identity(NetworkIdentity::new(
        "qomicex-local-relay".to_string(),
        "qomicex-relay-secret".to_string(),
    ));
    cfg.set_hostname(Some("qomicex-local-relay".to_string()));
    cfg.set_dhcp(true);
    cfg.set_listeners(vec![
        format!("tcp://{listen}").parse()?,
        format!("udp://{listen}").parse()?,
    ]);
    let mut flags = cfg.get_flags();
    flags.no_tun = true;
    flags.use_smoltcp = true;
    flags.multi_thread = true;
    flags.latency_first = true;
    cfg.set_flags(flags);

    let manager = Arc::new(easytier::instance::factory::native_instance_manager());
    let id = manager.run_network_instance(cfg, ConfigFileControl::STATIC_CONFIG)?;
    for _ in 0..60 {
        if manager.instance(id).is_some_and(|i| i.is_ready()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!("中继已就绪，按 Ctrl+C 退出");

    if seconds > 0 {
        tokio::time::sleep(Duration::from_secs(seconds)).await;
    } else {
        tokio::signal::ctrl_c().await?;
    }
    manager.delete_network_instances([id]).await?;
    println!("中继已停止");
    Ok(())
}

async fn run_host(
    client: &ScaffoldingClient,
    ct: &CancellationToken,
    player_name: &str,
    machine_id: &str,
    port: u16,
) -> Result<(), Box<dyn Error>> {
    println!("创建房间...");
    let center = client
        .create_room(
            player_name.to_string(),
            machine_id.to_string(),
            "Qomicex".into(),
            port,
            ct.clone(),
        )
        .await?;
    println!("房间码: {}", center.room_code().raw());
    println!("按 Ctrl+C 退出");

    let mut rx = center.players_changed_rx();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let players = rx.borrow().clone();
            println!("玩家数: {}", players.len());
        }
    });

    tokio::signal::ctrl_c().await?;
    center.close(ct.clone()).await?;
    Ok(())
}

async fn run_guest(
    client: &ScaffoldingClient,
    ct: &CancellationToken,
    room_code: &str,
    player_name: &str,
    machine_id: &str,
) -> Result<(), Box<dyn Error>> {
    println!("加入房间 {room_code}...");
    let guest = client
        .join_room(
            room_code,
            player_name.to_string(),
            machine_id.to_string(),
            "Qomicex".into(),
            Vec::new(),
            ct.clone(),
        )
        .await?;
    println!("已加入!");

    let (mc_host, mc_port) = guest.map_minecraft_port(ct.clone()).await?;
    println!("MC 服务器地址: {mc_host}:{mc_port}");

    let players = guest.get_player_list().await?;
    println!("玩家 ({}):", players.len());
    for pl in players {
        println!("  {}", pl.name);
    }
    println!("按 Ctrl+C 退出");

    tokio::signal::ctrl_c().await?;
    guest.leave().await;
    Ok(())
}
