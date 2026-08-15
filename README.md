# Qomicex Connector Rust

基于 [EasyTier](https://github.com/EasyTier/EasyTier) 库（非进程）的 Minecraft 联机内网穿透库，
是 [Qomicex.Connector.Part.Scaffolding](https://github.com/Qomicex-Public/Qomicex.Connector.Part.Scaffolding)（C#）的 Rust 移植。

房主（Host）一键开房生成房间码，好友（Guest）凭房间码即可加入并连接到房主的 Minecraft 服务器，
无需公网 IP、无需手动配置端口映射。EasyTier 以 **Rust 库内嵌**方式运行（不再 spawn 外部进程），
无需管理员权限（smoltcp 用户态协议栈，无 TUN 依赖）。

本仓库同时提供：

- **`qomicex-connector`** —— 可被任意 Rust 程序引用的库（本文档主角）。
- **`qomicex-connector-cli`** —— 命令行程序（`relay` / `host` / `guest` 子命令）。

## 架构定位（重要）

本库是**符合标准 SCF 协议的联机库**：只实现协议与提供**拓展接口**，**不内置任何业务功能**。
踢人、黑名单、重连审核等"房主管理"能力不属于 SCF 协议，一律由**调用方**（如 Qomicex 启动器 backend）
通过本库的拓展接口自行实现。判断标准：改的是协议/接口 → 进本库；是业务功能 → 放调用方。

## 一、工作原理

联机遵循社区 **SCF 标准协议**（命名空间 `c`，代表 community），核心流程：

1. **加入虚拟网络**：房主与房客用同一房间码派生出的 `network-name` / `network-secret` 加入同一个 EasyTier 网络。
2. **中心发现**：房主以主机名 `scaffolding-mc-server-<TCP端口>` 广播自己；房客扫描网络节点，用正则识别出联机中心的虚拟 IP 与端口。
3. **建立信息通道**：房客先尝试**直连**房主虚拟 IP；若不可达，则动态添加 `127.0.0.1` 端口转发再连（运行时 `apply_config_patch`，不重启 EasyTier、不断开已有连接；P2P 打洞需多轮重试）。
4. **协议协商**：房客发送 `c:protocols` 报告自身支持的协议，与房主取交集得到本次可用协议集合。
5. **获取服务器地址**：房客发送 `c:server_port` 拿到 MC 端口，`map_minecraft_port` 返回最终可连接的 `host:port`。
6. **心跳保活**：房客每 5 秒发送一次 `c:player_ping`，房主据此维护玩家列表并检测超时（15 秒）。

### 房间码格式

```
U/XXXX-XXXX-XXXX-XXXX
```

由 34 进制字符（去除易混淆的 I/O）组成，含除 7 校验。前半段派生虚拟网络名，后半段作为网络密钥。

### 内置标准协议（命名空间 `c`）

| 协议键 | 作用 |
| --- | --- |
| `c:ping` | 连通性测试（原样回显请求体） |
| `c:protocols` | 协议协商，返回中心支持的协议列表 |
| `c:server_port` | 返回 MC 服务器端口（大端 u16；状态码 32 表示服务器未启动） |
| `c:player_ping` | 玩家心跳（5 秒一次） |
| `c:player_profiles_list` | 返回玩家列表（含 HOST/GUEST） |
| `c:player_easytier_id` | 协议协商标识位（决定心跳/列表是否携带 easytier_id） |

> 标准协议由 SCF 规范锁定，本库完整实现且不可更改。自定义协议请使用**独立命名空间**（如 `qml`）以避免冲突。

## 二、环境准备

- Rust 1.95+（本机验证 1.97.1）
- **protoc**（prost 编译 easytier-proto 需要）：`winget install Google.Protobuf`
- **7-Zip + VC-LTL + YY-Thunks**（thunk-rs 构建 easytier 的 Windows 兼容层需要，首次构建自动下载）
- **Git SSH**（easytier 依赖走 SSH）

## 三、构建

```bash
# 全 workspace 检查
cargo check

# 单元测试（46 个：RoomCode / 序列化帧互操作 golden / 协商 / 发现 / 协议处理器 / 中继节点 / player_ping 裁决契约）
cargo test -p qomicex-connector

# 端到端集成测试（需要真实 EasyTier 实例，默认跳过）
cargo test -p qomicex-connector --test e2e_direct -- --ignored --nocapture

# Release CLI
cargo build -p qomicex-connector-cli --release
```

## 四、CLI 使用

```bash
# 本地中继（同机测试；跨机器+公网中继可跳过）
qomicex-connector-cli relay --listen 0.0.0.0:11010

# 房主开房（--relay 可重复指定；不传则在线获取节点）
qomicex-connector-cli --relay tcp://<中继IP>:11010 host 25565 Steve

# 房客加入
qomicex-connector-cli --relay tcp://<中继IP>:11010 guest U/XXXX-XXXX-XXXX-XXXX Alex
```

- `--relay` 支持任意 easytier peer URL：`tcp://`、`udp://`、`wss://`、`wg://`、`kcp://`、`quic://`、`faketcp://`
- 兼容原版 EasyTier 公共节点全部协议端口（tcp/udp 11010、wss 11011/11012、wg 11013）
- 注意：EasyTier 使用 smoltcp 用户态协议栈（`--no-tun`），**不支持 127.0.0.1 回环**；
  中继须监听局域网/公网 IP，且 P2P 打洞需真实跨机器网络环境才能建立数据面。

## 五、库 API 示例

```rust
use qomicex_connector::client::ScaffoldingClient;
use qomicex_connector::util::CancellationToken;

let ct = CancellationToken::new();
let client = ScaffoldingClient::new(None, None, None, None);

// Host（第 7 参数为可选的 player_ping 裁决钩子，见"拓展接口"；None = 标准 SCF 行为）
let center = client.create_room("Steve".into(), "machine-id".into(), "qml".into(), 25565, ct.clone(), vec![], None).await?;
let room_code = center.room_code().raw().to_string();

// Guest
let guest = client.join_room(&room_code, "Alex".into(), "machine-id2".into(), "qml".into(), vec![], ct.clone()).await?;
let (mc_host, mc_port) = guest.map_minecraft_port(ct.clone()).await?;
let players = guest.get_player_list().await?;
```

## 六、自定义协议（Host 端注册）

```rust
use std::sync::Arc;
use qomicex_connector::protocols::{DelegateProtocol, ProtocolHandler};

let proto: Arc<dyn ProtocolHandler> = Arc::new(
    DelegateProtocol::new_json("qml:game_version", || "1.20.1".to_string())
);
let center = client.create_room(/* ... */, ct.clone(), vec![proto], None).await?;
```

## 七、拓展接口（业务功能由调用方实现）

本库为调用方提供的全部"钩子 + 能力"，组合它们即可实现踢人/黑名单/重连审核等业务功能，
库内零业务代码：

| 接口 | 说明 |
| --- | --- |
| `ScaffoldingCenter::set_player_ping_handler` / `create_room(..., player_ping_handler)` | `c:player_ping` 裁决钩子（须在 `start` 前注入）。返回 `false` → 响应状态 255（不刷新心跳 → 15s 心跳超时兜底剔除）；返回 `true` 且不委托 → 保持连接不入列。**入列与否由调用方闭包决定** |
| `ScaffoldingCenter::handle_player_ping(info)` | 标准 SCF 入列行为（更新/加入玩家 + 通知），供调用方闭包在未命中自定义逻辑时委托 |
| `ScaffoldingCenter::disconnect_machine(machine_id)` | 按 machine_id 定向断开 SCF TCP 连接 |
| `ScaffoldingCenter::machine_source_ip(machine_id)` | 查询 SCF TCP 连接源 IP（反查 easytier peer 用） |
| `ScaffoldingCenter::easy_tier_nodes()` | 当前网络全部节点快照（hostname / 虚拟 IP / peer id） |
| `ScaffoldingCenter::disconnect_peer(peer_id)` | 断开与指定 easytier peer 的全部连接 |
| `ScaffoldingCenter::remove_player(machine_id)` / `get_players()` | 玩家列表维护 |

示例：房主侧踢人 + 重连审核（调用方实现）：

```rust
use qomicex_connector::center::scaffolding_center::ScaffoldingCenter;
use qomicex_connector::models::player::PlayerInfo;
use std::sync::{Arc, RwLock};

// 调用方自己的黑名单状态（业务数据）
let kicked: Arc<RwLock<std::collections::HashMap<String, bool>>> = Arc::new(RwLock::new(HashMap::new()));
let kicked2 = kicked.clone();
let handler = Arc::new(move |info: PlayerInfo| {
    let blacklisted = kicked2.read().unwrap().get(&info.machine_id).copied().unwrap_or(false);
    if blacklisted {
        return false; // 拒绝：状态 255，不刷新心跳
    }
    // 委托标准入列需要 center 句柄（建房后回填的 slot），此处省略
    true
});
let center = client.create_room(/* ... */, ct.clone(), vec![], Some(handler)).await?;
```

## 八、许可证

[GPLv3](LICENSE) — 本项目为自由软件，你可以自由分发与修改，但衍生作品必须同样以 GPL 兼容许可证发布并开放源代码。
