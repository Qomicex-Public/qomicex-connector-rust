# 移植日志：联机中心 TCP 服务器（TcpServer）

## 来源与目标

| 源文件（C#） | 目标文件（Rust） |
| --- | --- |
| `Qomicex.Connector/Center/TcpServer.cs` | `src/center/tcp_server.rs`（TcpServer 主体：结构、构造、start / stop / port） |
| — | `src/center/client_conn.rs`（拆分：ClientRegistry + handle_client + heartbeat_timeout_loop + notify_disconnected，超长拆分） |
| — | `src/center/mod.rs`（`mod client_conn;` + `pub mod tcp_server;`） |
| — | `lib.rs` 增加 `pub mod center;` |

## 新增依赖

无。`tokio`（net / io-util / time / sync / macros）、`serde_json`、`log` 已在 workspace 依赖中。

## 映射说明（一一对应）

| C# | Rust | 说明 |
| --- | --- | --- |
| `TcpServer(int port, ILogger logger, IReadOnlyList<IProtocol> protocols)` | `TcpServer::new(port: u16, protocols: Vec<Arc<dyn ProtocolHandler>>, disconnected_tx: UnboundedSender<String>)` | 见决策 1；日志由 log crate 全局处理 |
| `_protocols.ToDictionary(p => p.ProtocolKey)` | `protocols.into_iter().map(|p| (p.key().to_string(), p)).collect()` | 协议按 key 索引 |
| `event Action<string?>? ClientDisconnected` | `disconnected_tx: mpsc::UnboundedSender<String>`（构造注入） | 见决策 1 |
| `IsRunning => _listener is not null` | `is_running()` → `listener.is_some()` | 一致 |
| `Port => LocalEndpoint?.Port ?? _port` | `port()` → 启动时更新 `self.port` 为实际绑定端口 | 启动前返回构造端口 |
| `StartAsync(ct)` + `CreateLinkedTokenSource(ct)` | `start(&mut self, ct)` + `self.cts = ct.clone()` | 直接持有传入令牌 |
| `new TcpListener(IPAddress.Any, _port); Start()` | `TcpListener::bind(([0, 0, 0, 0], port)).await` | 0.0.0.0 全接口监听；绑定失败 → `Protocol` |
| `Task.Run(HeartbeatTimeoutLoop)` | `tokio::spawn(heartbeat_timeout_loop(...))` | 心跳任务 |
| `while (!_cts.IsCancellationRequested) { await AcceptTcpClientAsync(ct) }` | `select! { _ = ct.cancelled() => break, accepted = listener.accept() => ... }` | 取消可中断 accept |
| `_ = Task.Run(() => HandleClientAsync(client, ct))` | `tokio::spawn(async move { handle_client(...).await })` | 每连接一个任务 |
| `HandleClientAsync` | `client_conn::handle_client`（free fn） | 见决策 6 |
| `ProtocolSerializer.DeserializeRequest(stream)` | `deserialize_request_async(&mut stream).await` | 现有 API 直引 |
| `$"{request.Namespace}:{request.RequestType}"` | `format!("{}:{}", namespace, request_type)` | 一致 |
| `_protocols.TryGetValue(key, out handler)` | `protocols.get(&key)` | 一致 |
| `await handler.HandleAsync(request, ct)` | `handler.handle(&request).await` | `Arc<dyn ProtocolHandler>` 现有 trait |
| 未命中 → `ProtocolResponse { Status = 255, Body = Encoding.UTF8.GetBytes($"未知协议: {key}") }` | `ProtocolResponse { status: 255, body: format!("未知协议: {key}").into_bytes() }` | 一致 |
| `ProtocolSerializer.SerializeResponse(response)` + `stream.WriteAsync` | `serialize_response(&response)` + `stream.write_all(&bytes).await` | 一致 |
| `key == "c:player_ping" && response.IsSuccess` → 记录心跳 + JSON 解析 `machine_id` | 相同条件 → `last_heartbeat.insert` + `serde_json::Value` 手动取 `machine_id`（`as_str().unwrap_or("")`） | `GetString() ?? ""` 语义一致，`try/catch {}` → `if let Ok` |
| `catch (Exception ex) when (ex is not OperationCanceledException)` → LogWarning | 读/写错误 → `warn!("客户端 {client_id} 处理异常: {message}")` | 消息一致 |
| `finally { TryRemove 三个字典; ClientDisconnected?.Invoke(machineId); client.Dispose(); }` | 任务尾部清理三个字典 → `notify_disconnected(tx, machine_id)` → 函数返回时 stream drop | 见决策 3 / 4 |
| `HeartbeatTimeoutLoop`：5s 延迟 + 15s 超时 | `heartbeat_timeout_loop`：`select! { ct.cancelled(), sleep(HEARTBEAT_INTERVAL) }` + `Instant` 差值 > `HEARTBEAT_TIMEOUT` | 见决策 2 |
| 超时 → `timedOutClient.Close(); Dispose()` | 超时 → 取消该连接的 `CancellationToken` | 见决策 2 |
| `Stop()`：`_cts.Cancel()` + `_listener.Stop()` + Close 全部客户端 | `stop()`：`cts.cancel()` + `listener.take()` | 见决策 4 |
| `Dispose()` / `GC.SuppressFinalize` | 未移植 | Rust 无 Dispose 惯例，字段随 struct drop |

## 偏差 / 决策记录

1. **事件机制（决策）**：C# `event Action<string?>?` → `tokio::sync::mpsc::UnboundedSender<String>`，构造时注入。约定：`Some(machine_id)` 发送 machine_id，`None` 发送空串，接收方把空串映射回 `None`。选择 mpsc 而非 watch（多消费者/无快照需求）而非轮询队列（sender 语义更贴近 C# 事件）。
2. **超时断开机制（决策）**：采用"每连接一个 `CancellationToken`"方案——`active_clients: HashMap<String, CancellationToken>`，心跳超时按 client_id 取出令牌并 `cancel()`，`handle_client` 内 `select! { biased; server_ct, conn_ct, read }` 监听。放弃"`active_clients` 存 `TcpStream` 直接 close"（tokio 中跨任务 close 需 Arc 包装流并可能打断写响应，令牌更干净）。
3. **断开事件单次触发（偏差）**：C# 心跳超时路径会触发**两次** `ClientDisconnected`（心跳循环内一次带 machine_id，`HandleClientAsync` finally 一次带 null）。Rust 统一由 `handle_client` 清理段单次触发：心跳循环只移除心跳记录 + 取消令牌，**不**提前移除 machine_id 映射、**不**直接发事件，保证事件携带正确 machine_id 且不重复。
4. **Stop 关闭连接语义（决策）**：C# `Stop()` 直接 `Close()` 全部 `_activeClients`；Rust 取消服务器令牌后所有连接任务经 select! 退出并释放流，行为等价，且同样会经清理段触发断开事件（C# 也会）。
5. **重复 start 防护（偏差）**：C# 二次 `StartAsync` 会二次绑定抛 `SocketException`；Rust 显式返回 `Protocol("TCP 服务已启动")`。
6. **`handle_client` / `heartbeat_timeout_loop` 拆分为 free fn（决策）**：因 ≤200 行/文件约束拆至 `client_conn.rs`；均以参数显式传入所需状态（protocols 表整体浅克隆共享 `Arc`），不访问 `TcpServer` 私有字段。
7. **stop 签名（约束）**：按约束保持 `pub fn stop(&mut self)`（start 的 accept 循环持有 `&mut self` 借用，需在 start 返回后调用，或由调用方以共享所有权方式持有；记录供 ScaffoldingCenter 移植时参考）。
8. **取消检查时机（偏差）**：取消在 select! 循环顶检查（biased），即正在处理中的请求会先完成再退出；C# 靠 `Close()` 立即中断阻塞读。行为差异可忽略（最多一个请求的延迟）。

## 未覆盖（UNMAPPED 汇总）

- `ILogger<T>` 结构化日志参数 → log crate 文本日志
- `CreateLinkedTokenSource` 链接令牌 → 直接持有传入令牌（cts 字段与 start 参数相同）
- `ObjectDisposedException` 防护 / `Dispose()` / `GC.SuppressFinalize` → Rust 无对应
- `_client.Connected` 实时 socket 状态检查 → 依赖读写失败与取消发现
- 心跳超时双触发事件（C# 行为，Rust 有意收敛为单次，见决策 3）

## 依赖约定（并行 subagent 协调）

- 引用 `crate::core::protocol_serializer::{deserialize_request_async, serialize_response}`（已有签名）。
- `lib.rs` 中 `pub mod center;` 已由本 subagent 添加（与 `core` 等模块并列）。
- 供 ScaffoldingCenter 使用：`TcpServer::new(port, protocols, disconnected_tx)` / `start(ct)` / `stop()` / `port()` / `is_running()`。
