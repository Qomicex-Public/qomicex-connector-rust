# 移植日志：Guest TCP 客户端（TcpClient）

## 来源与目标

| 源文件（C#） | 目标文件（Rust） |
| --- | --- |
| `Qomicex.Connector/Guest/TcpClient.cs` | `src/guest/tcp_client.rs` |
| — | `src/guest/mod.rs`（新增模块根，lib.rs 挂载 `pub mod guest;`） |

## 新增依赖

无。`tokio`（net / io-util / time / sync）已在 workspace 依赖中。

## 映射说明（一一对应）

| C# | Rust | 说明 |
| --- | --- | --- |
| `TcpClient(ILogger<TcpClient>)` | `TcpClient::new()` | 无参；日志由 log crate 全局处理 |
| `SemaphoreSlim _sendLock = new(1, 1)` | `send_lock: tokio::sync::Mutex<()>` | 保证单发单收 |
| `System.Net.TcpClient? _client` / `NetworkStream? _stream` | `stream: Option<TcpStream>` | 合并为单一可选流 |
| `IsConnected => _client?.Connected == true` | `is_connected()` → `stream.is_some()` | 见偏差 4 |
| `ConnectAsync(host, port, ct)` | `connect(&mut self, host: &str, port: u16)` | 见决策 1（取消移除）；失败 → `CenterConnection` |
| `_stream.ReadTimeout = 15000` | `tokio::time::timeout(READ_TIMEOUT, deserialize_response_async(stream))` | 15s 读超时包住每次响应读取 |
| `SendAsync(request, ct)` | `send(&mut self, request: &ProtocolRequest)` | 加锁 → 序列化 → write_all → flush → 读响应 |
| `throw new CenterConnectionException("未连接到联机中心")` | `Err(CenterConnection("未连接到联机中心"))` | 一致 |
| `LogInformation("已连接到中心: {Host}:{Port}")` | `log::info!("已连接到中心: {host}:{port}")` | 文案一致 |
| `LogInformation("发送: {Key}, {N} 字节")` | `log::info!("发送: {key}, {} 字节", body.len())` | Key = `Namespace:RequestType` |
| `LogDebug("发送原始数据: {Len} 字节")` | `log::debug!("发送原始数据: {} 字节", ...)` | 一致 |
| `LogWarning("请求 {Key} 返回错误状态: {Status}")` | `log::warn!("请求 {key} 返回错误状态: {}", status)` | `!response.is_success()` 时记录 |
| `catch (Exception) { LogError("TCP 发送/接收失败: {Key}"); throw; }` | 错误分支 `log::error!("TCP 发送/接收失败: {key}: {e}")` 后返回 Err | 见决策 2 |
| `Disconnect()` | `disconnect()`：`stream = None` + `info!("已断开连接")` | 见偏差 3 |
| `Dispose()` | `impl Drop`：流存在则断开 + 日志 | 见偏差 3 |

## 依赖约定（并行 subagent 协调）

- 引用 `crate::core::protocol_serializer::{serialize_request, deserialize_response_async}`，按约定签名：
  - `pub fn serialize_request(&ProtocolRequest) -> Vec<u8>`
  - `pub async fn deserialize_response_async<R: AsyncRead + Unpin>(&mut R) -> Result<ProtocolResponse, ScaffoldingError>`
- `lib.rs` 中 `pub mod guest;` 已由本 subagent 添加；`pub mod core;` **由 core subagent 负责添加**，本 subagent 未添加（避免竞争）。若 core subagent 未处理，请主控统一补挂。
- 若 core 实际签名与约定不符，由主控协调修正本文件调用点。

## 偏差 / 决策记录

1. **CancellationToken 移除（约束 #6 最终决定）**：`connect` / `send` 均不接受取消参数，取消与超时由调用方用 `tokio::select!` 包裹实现，降低耦合。C# `ConnectAsync(host, port, ct)` 与 `SendAsync(request, ct)` 的 ct 参数未移植。
2. **错误映射收敛**：C# 连接失败抛 `SocketException`、写失败抛 `SocketException/IOException`、读超时抛 `IOException`，均非 `CenterConnectionException`；Rust 侧按约束统一映射为 `ScaffoldingError::CenterConnection(msg)`（消息携带主机端口 / 错误原因），`deserialize_response_async` 的序列化错误则原样传播。
3. **`Disconnect` / `Dispose` 日志去重**：C# 每次调用 `Disconnect()` 都打日志（`Dispose()` 重复调用会重复打）。Rust 版仅在流存在时断开并记录，避免重复日志（轻微偏差，行为更干净）。
4. **`IsConnected` 语义弱化**：C# `_client.Connected` 反映真实 socket 状态（对端断开后可能变 false）；Rust 版仅检查 `stream.is_some()`，无法感知对端断开，需依赖后续 `send` 失败发现。

## 未覆盖（UNMAPPED 汇总）

- CancellationToken 取消语义（见决策 1）
- `ILogger<T>` 结构化日志参数（Rust 用 log crate 文本日志替代）
- `_client.Connected` 实时 socket 状态检测（见偏差 4）
- `CenterConnectionException` 独立类型（并入 `ScaffoldingError::CenterConnection`）

# 移植日志：访客端联机客户端（ScaffoldingGuest）

## 来源与目标

| 源文件（C#） | 目标文件（Rust） |
| --- | --- |
| `Qomicex.Connector/Guest/ScaffoldingGuest.cs` | `src/guest/scaffolding_guest.rs` |
| — | `src/guest/mod.rs`（追加 `pub mod scaffolding_guest;`） |

## 新增依赖

无。`tokio`（sync / time / net）、`serde`、`serde_json`、`log` 均为 crate 既有依赖。

## 映射说明（一一对应）

| C# | Rust | 说明 |
| --- | --- | --- |
| 构造函数注入 EasyTierManager / TcpClient / CenterDiscoveryService | `new(player_name, machine_id, vendor, custom_protocol_keys, relay_nodes)` | 内部创建 `Arc<Mutex<...>>` 包装的 EasyTierManager / TcpClient；发现逻辑直接用 `core::center_discovery::discover` |
| `event Action? ConnectionLost` | `connection_lost_tx: watch::Sender<bool>` + `connection_lost_rx()` | watch channel 订阅替代事件 |
| `NegotiatedProtocols` 属性 | `negotiated_protocols()` → `RwLock` 读 clone | async |
| `MinecraftHost / MinecraftPort` 属性 | `minecraft_host()` / `minecraft_port()` | async getter |
| `ConnectAsync(code, ct)` | `connect(&self, code, ct)` | 建 config → start EasyTier → discover → 直连或动态转发 → 心跳；见决策 1、9 |
| `TryConnectOnceAsync` | `try_connect_once` | 超时包 connect，ping/协商串行在外；见决策 2 |
| `TryConnectWithRetryAsync` | `try_connect_with_retry` | 10 次 × 3s 超时，间隔 2s |
| `SendPlayerPing(ct)` | `send_player_ping` | 未连接 → 发送 `true`；协商含 `c:player_easytier_id` 才填 easytier_id；锁逐一获取（tcp → negotiated → easy_tier → tcp），见决策 3 |
| `NegotiateProtocols(ct)` | `negotiate_protocols` | 标准 6 协议 + 自定义，`\0` 连接；成功才写 `negotiated` |
| `PingAsync` | `ping` | body `[0x42]`，返回 `is_success()` |
| `GetServerPortAsync` | `get_server_port` | 32..64 → "MC 服务器未启动"；非成功 → 失败；body 2 字节大端 → u16；长度非法补 Protocol 错误（C# 会抛 ArgumentException） |
| `MapMinecraftPortAsync` | `map_minecraft_port` | 直连模式直接缓存返回；转发模式动态 ADD MC 转发（不重启 EasyTier，Center TCP 与心跳不断）；见决策 4、9 |
| `GetPlayerListAsync` | `get_player_list` | `serde_json::Value` 数组解析；`kind == "HOST"` → Host，否则 Guest |
| `SendAsync(request, ct)` | `send_raw` | 透传 tcp.send |
| `SendAsync<TResp>(key, ct)` | `send_json` | split_key 拆 ns/type；空 body；非成功 → Protocol 错误；JSON 反序列化 |
| `SendAsync<TReq, TResp>(key, payload, ct)` | `send_json_req` | 同上，body = JSON 序列化 payload |
| `LeaveAsync(ct)` | `leave` | 停心跳 → 断开 → 停 EasyTier |
| `StartAsync(ct)` 心跳 | `start_heartbeat` | HeartbeatService 回调不调用 self 方法，全部克隆 Arc，避免锁重入；见决策 5 |
| `SplitKey` | `split_key` | 私有函数；无 `:` → Protocol 错误 |
| `ParseLocalPortFromForward` | `parse_forward_addrs` | 返回 `(bind_addr, dst_addr)` 供 `apply_config_patch` 构造 `PortForwardConfigPb`；原 C# 仅取本地端口（已随决策 9 变更） |
| `FindFreeLocalPort()` | `find_free_local_port` | `TcpListener::bind("127.0.0.1:0")` 取端口后立即 drop |

## 偏差 / 决策记录

1. **锁顺序（ConnectAsync 转发分支，原重启方案已废弃，见决策 9）**：历史版本先 `easy_tier.stop()`（块内释放），再改 `config.port_forwards`（块内释放），最后克隆 config 并 `start`。全程一次只持一个锁，杜绝锁重入死锁。
2. **TryConnectOnceAsync 死锁规避**：C# 在同一方法内连续 Connect → SendPlayerPing → NegotiateProtocols；Rust 版将 `tcp.lock().await.connect(...)` 整体放入 `tokio::time::timeout`（守卫随语句结束释放），ping / 协商在闭包外串行调用，每次各自短暂持锁。
3. **SendPlayerPing 锁顺序**：先查 `tcp.is_connected()`（锁即释放）→ 读 `negotiated`（释放）→ 取 `easy_tier.node_id()`（释放）→ 最后 `tcp.send`。禁止同时持有两个锁。
4. **heartbeat_ct 字段**：新增 `tokio::sync::Mutex<Option<CancellationToken>>` 记录心跳取消令牌；`stop_heartbeat()` 取出并 cancel，替代 C# `_heartbeatService?.Dispose()`。
5. **心跳回调无 self 调用**：回调闭包内全部使用 `start_heartbeat` 时克隆的 `Arc<Mutex<...>>` + `watch::Sender` + String，与 `send_player_ping` 逻辑一致但独立实现（防重入）。
6. **`connection_lost` 触发语义**：C# 用 `_connectionLostFired` 防重复触发；Rust 用 `watch::Sender::send(true)`（重复 send 幂等，值不变不唤醒），行为等价且更简单。
7. **心跳 body 构造**：用 `serde_json::json!` 直接构造（`easytier_id` 为 null 或字符串，`kind` 为 null），与 C# `PlayerProfileEntry` 序列化结果一致（System.Text.Json 默认序列化 null）。
8. **`GetServerPortAsync` 长度非法分支**：C# `ReadUInt16BigEndian` 对非 2 字节抛异常；Rust 补 `Protocol("服务器端口响应长度非法")` 错误。
9. **端口转发改为运行时 patch（替代重启，2026-08-11）**：原方案在直连失败 / `map_minecraft_port` 时 stop/start 重启 EasyTier（P2P 连接重建、Center TCP 断开重连、心跳停启）。现改为 `EasyTierManager::apply_config_patch` 动态 ADD 端口转发规则：
   - 底层 `easytier_core::management::apply_config_patch`（进程内直接调用，无 RPC）；
   - 实例保持运行，网络连接、Center TCP、心跳全程不断；`map_minecraft_port` 不再调用 `stop_heartbeat`/`tcp.disconnect`/`stop`/`start`/重连；
   - `self.config.port_forwards` 存储仍同步更新（增量 push），保持配置一致；
   - `ct` 参数在 `map_minecraft_port` 中不再使用，保留签名兼容改为 `_ct`；
   - 新增 `apply_port_forward_patch` 私有辅助 + `parse_forward_addrs`（替代 `parse_local_port_from_forward`）；
   - 依赖前提：`management` + `proxy-smoltcp-stack` feature（workspace 已启用）；实例须处于 Running。

## 未覆盖（UNMAPPED 汇总）

- C# `Dispose()`（Rust 无生命周期资源需手动释放，Arcs 随结构体 drop 自动回收）
- `ILogger<T>` 结构化日志参数（Rust 用 log crate 文本日志替代）
- 取消参数逐方法透传（统一由调用方传 `CancellationToken`，connect/retry 内检查 `is_cancelled`）
