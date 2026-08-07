# Qomicex Connector Rust — 构建与开发指南

## 前置依赖（Windows）

- Rust 1.95+（workspace `rust-version` 要求；本机验证 1.97.1）
- **protoc**（prost 编译 easytier-proto 需要）：`winget install Google.Protobuf`，路径写入 `.cargo/config.toml` 的 `PROTOC`
- **7-Zip + VC-LTL + YY-Thunks**（thunk-rs 构建 easytier 的 Windows 兼容层需要）：
  - 首次构建会自动从 GitHub 下载 VC-LTL-Binary.7z / YY-Thunks-Objs.zip 并解压
  - 网络受限时手动下载解压，并用 `.cargo/config.toml` 的 `VC_LTL` / `YY_THUNKS` 环境变量指向本地路径
- **Git SSH**：easytier 依赖走 SSH（libgit2 直连可能失败），已配置 `net.git-fetch-with-cli = true`

## 构建

```bash
# 全 workspace 检查
cargo check

# 单元测试（35 个：RoomCode / 序列化帧互操作 golden / 协商 / 发现 / 协议处理器 / 中继节点）
cargo test -p qomicex-connector

# Release CLI（含 relay / host / guest 子命令）
cargo build -p qomicex-connector-cli --release
```

## CLI 使用

```bash
# 本地中继（同机测试；跨机器+公网中继可跳过）
qomicex-connector-cli relay --listen 0.0.0.0:11010

# 房主开房（--relay 可重复指定；不传则在线获取节点）
qomicex-connector-cli --relay tcp://<中继IP>:11010 host 25565 Steve

# 房客加入
qomicex-connector-cli --relay tcp://<中继IP>:11010 guest U/XXXX-XXXX-XXXX-XXXX Alex
```

注意：EasyTier 使用 smoltcp 用户态协议栈（`--no-tun`），**不支持 127.0.0.1 回环**；
中继须监听局域网/公网 IP，且 P2P 打洞需真实跨机器网络环境才能建立数据面。

## 库 API 示例

```rust
use qomicex_connector::client::ScaffoldingClient;
use qomicex_connector::util::CancellationToken;

let ct = CancellationToken::new();
let client = ScaffoldingClient::new(None, None, None, None);

// Host
let center = client.create_room("Steve".into(), "machine-id".into(), "qml".into(), 25565, ct.clone(), vec![]).await?;
let room_code = center.room_code().raw().to_string();

// Guest
let guest = client.join_room(&room_code, "Alex".into(), "machine-id2".into(), "qml".into(), vec![], ct.clone()).await?;
let (mc_host, mc_port) = guest.map_minecraft_port(ct.clone()).await?;
let players = guest.get_player_list().await?;
```

## 自定义协议（Host 端注册）

```rust
use std::sync::Arc;
use qomicex_connector::protocols::{DelegateProtocol, ProtocolHandler};

let proto: Arc<dyn ProtocolHandler> = Arc::new(
    DelegateProtocol::new_json("qml:game_version", || "1.20.1".to_string())
);
let center = client.create_room(/* ... */, ct.clone(), vec![proto]).await?;
```

## 依赖与配置要点

- easytier / easytier-core / easytier-proto：git 依赖 `ssh://git@github.com/Qomicex-Public/EasyTier4QML.git`，锁定 `rev = "287c667"`
- easytier features：`aes-gcm, endpoint-discovery, extended-services, management, tcp-hole-punch, zstd, smoltcp, kcp, quic, faketcp`
- **faketcp 链接依赖**：easytier build.rs 以相对路径 `easytier/third_party/x86_64/` 搜索 `Packet.lib`（按 rustc CWD=workspace 根解析）→ 已复制到工作区根 `easytier/third_party/x86_64/`，删除会导致 `LNK1181: Packet.lib` 链接失败
- 数据库/服务端：无
