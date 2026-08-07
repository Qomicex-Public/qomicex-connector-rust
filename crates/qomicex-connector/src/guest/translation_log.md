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
