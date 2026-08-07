# 翻译记录：Qomicex.Connector.Console (C#) → qomicex-connector-cli (Rust)

- 源文件：`scaffolding-src/Qomicex.Connector.Console/Program.cs`
- 目标文件：`crates/qomicex-connector-cli/src/main.rs`
- 日期：2026-08-07

## 依赖新增（需主控合并到 crates/qomicex-connector-cli/Cargo.toml）

```toml
[dependencies]
clap = { version = "4", features = ["derive"] }
uuid.workspace = true
tokio.workspace = true  # 需追加 feature: "signal"（tokio::signal::ctrl_c 依赖）
```

- `uuid` 在 workspace 已声明（`features = ["v4"]`），cli crate 直接引用 `uuid.workspace = true`。
- workspace 的 tokio 声明不含 `signal` feature，cli crate 需 `tokio = { workspace = true, features = ["signal"] }`。

## API 适配差异

| C# | Rust | 说明 |
|---|---|---|
| `Guid.NewGuid().ToString("N")[..12]` | `uuid::Uuid::new_v4().simple().to_string()[..12]` | 语义一致：v4 随机 UUID 的 32 位十六进制取前 12 位 |
| `new ScaffoldingClient(null, loggerFactory)` | `ScaffoldingClient::new(None, None, None, None)` | 不覆盖中继节点、无自定义 UA/地区 |
| `await client.CreateRoomAsync(playerName, machineId, "Qomicex", port)` | `client.create_room(player_name, machine_id, "Qomicex", port, ct).await?` | 多一个 `CancellationToken` 参数 |
| `center.RoomCode.Raw` | `center.room_code().raw()` | 房间码读取方式不同 |
| `await client.JoinRoomAsync(code, playerName, machineId, "Qomicex")` | `client.join_room(code, player_name, machine_id, "Qomicex", Vec::new(), ct).await?` | 多 `custom_protocol_keys`（空）与 `CancellationToken` |
| `guest.MapMinecraftPortAsync()` | `guest.map_minecraft_port(ct.clone()).await?` | 返回 `(String, u16)` 元组 |
| `guest.GetPlayerListAsync()` 遍历 `pl.Name` | `guest.get_player_list().await?` 遍历 `pl.name` | `PlayerInfo` 字段 snake_case |
| `await client.CloseAsync()` | `client.close_all(ct.clone()).await` | finally 语义对齐：无论成功失败均调用 |
| `await Task.Delay(-1)` | `tokio::signal::ctrl_c().await` | C# 无限等待，Rust 显式等 Ctrl+C |
| `case "host"/"create"`（`ToLower()` 后匹配） | clap 子命令 `host`（alias `create`）+ `#[command(ignore_case = true)]` | 覆盖 C# 的小写化匹配语义 |
| `int.TryParse(args[1])` 失败打印"无效端口"返回 1 | clap 接收 `String`，手动 `parse::<u16>()` 失败返回 `ScaffoldingError::Protocol("无效端口")`，main 打印"错误: 无效端口"退出码 1 | 保留中文提示；"错误: " 前缀来自统一错误路径 |
| `catch (Exception ex)` 打印 `ex.Message` 返回 1 | main 中 `Err(e) => println!("错误: {e}"); std::process::exit(1)` | `ScaffoldingError` 已实现 `Display`/`std::error::Error` |
| `LoggerFactory` MinimumLevel=Information | `env_logger::Builder::from_env(Env::default().default_filter_or("info"))` | 默认过滤级别对齐 Information |
| 无 | host 额外 spawn 任务订阅 `players_changed_rx`，watch 循环打印玩家数变化 | 任务要求的行为增强（C# 原版不打印玩家数） |
| 无 | host 结束前 `center.close(ct.clone()).await?` | 对齐 C# `using` 释放语义 |

## 已知差异（有意保留）

1. clap 参数缺失时打印使用说明并以退出码 2 退出（C# 为 1）——clap 默认行为。
2. `get_player_list` 玩家名打印带两空格缩进，与 C# `foreach` 输出一致。
3. `close_all` 在错误路径也会执行（对齐 C# `finally`），先清理后打印错误。
