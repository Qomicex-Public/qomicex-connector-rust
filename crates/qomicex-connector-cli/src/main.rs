use std::error::Error;

use clap::{Parser, Subcommand};

use qomicex_connector::client::ScaffoldingClient;
use qomicex_connector::error::ScaffoldingError;
use qomicex_connector::util::CancellationToken;

#[derive(Parser)]
#[command(name = "qomicex-connector-cli")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(alias = "create")]
    Host {
        mc_port: String,
        #[arg(default_value = "Qomicex-Player")]
        player_name: String,
    },
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
    let machine_id = uuid::Uuid::new_v4().simple().to_string();
    let machine_id = &machine_id[..12];

    let client = ScaffoldingClient::new(None, None, None, None);
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
