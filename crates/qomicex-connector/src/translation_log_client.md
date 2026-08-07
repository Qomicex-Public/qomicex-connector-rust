# 移植日志：连接器库入口客户端（ScaffoldingClient）

## 来源与目标

| 源文件（C#） | 目标文件（Rust） |
| --- | --- |
| `Qomicex.Connector/ScaffoldingClient.cs` | `src/client.rs` |
| — | `src/lib.rs`（追加 `pub mod client;`） |

## 新增依赖

无。`tokio`（sync）、`log` 均为 crate 既有依赖。

## 映射说明（一一对应）

| C# | Rust | 说明 |
| --- | --- | --- |
| 构造函数 `(easyTierPath, loggerFactory, relayNodes, additionalRelayNodes, userAgent, preferredRegion)` | `new(override_relay_nodes, additional_relay_nodes, user_agent, preferred_region)` | easyTierPath / loggerFactory 不移植（Rust 侧 EasyTier 为库模式、日志用 log crate），见 UNMAPPED |
| `_overrideRelayNodes` / `_additionalRelayNodes` | `override_relay_nodes` / `additional_relay_nodes: Option<Vec<String>>` | 字段一一对应 |
| `_cachedRelayNodes`（无锁字段） | `cached_relay_nodes: tokio::sync::Mutex<Option<Vec<String>>>` | C# 无并发访问（单线程 DI 使用）；Rust 加锁防并发调用 create/join |
| `_managedResources: List<IDisposable>` | `managed_centers: Mutex<Vec<Arc<ScaffoldingCenter>>>` + `managed_guests: Mutex<Vec<Arc<ScaffoldingGuest>>>` | 按类型拆分（见决策 1）；EasyTier/TcpClient 不再单独托管（见决策 2） |
| `ResolveRelayNodesAsync(ct)` | `resolve_relay_nodes()` | 缓存命中直接返回；override 分支 `RelayNodes.Resolve` → `nodes::resolve(Some(override), additional)`；否则 `RelayNodeProvider.FetchAsync(ct)` → `provider.fetch().await` → `nodes::resolve(Some(&fetched), additional)`；见决策 3 |
| `CreateRoomAsync(playerName, machineId, vendor, minecraftPort, customProtocols, ct)` | `create_room(player_name, machine_id, vendor, minecraft_port, ct)` | `customProtocols` 不移植（契约无此参数）；见 UNMAPPED |
| `RoomCode.Generate()` | `RoomCode::generate()` | 一致 |
| `LogInformation("创建房间: 端口 {Port}", ...)` | `log::info!("创建房间: 端口 {minecraft_port}")` | 文案一致 |
| `new ScaffoldingCenter(..., relayNodes)` + `StartAsync(ct)` | `ScaffoldingCenter::new(...) + center.start(ct).await?` | 按契约签名 `new(room_code, player_name, machine_id, vendor, minecraft_port, relay_nodes)`，relay_nodes 传 `Some(Vec<String>)` |
| `_managedResources.Add(center)` | `self.managed_centers.lock().await.push(center.clone())` | start 成功后才入列表（见决策 4） |
| `LogInformation("房间创建成功，房间码: {Code}", code.Raw)` | `log::info!("房间创建成功，房间码: {}", center.room_code().raw())` | 一致 |
| `JoinRoomAsync(roomCode, playerName, machineId, vendor, customProtocolKeys, ct)` | `join_room(room_code_str, player_name, machine_id, vendor, custom_protocol_keys, ct)` | 参数一一对应 |
| `RoomCode.Parse(roomCode)`（抛异常） | `RoomCode::parse(room_code_str)?` | 错误向上传播（`ScaffoldingError::RoomCodeInvalid`） |
| `LogInformation("加入房间: {Code}")` | `log::info!("加入房间: {}", code.raw())` | 一致 |
| `new ScaffoldingGuest(...)` + `ConnectAsync(code, ct)` | `ScaffoldingGuest::new(...) + guest.connect(&code, ct).await?` | 按实际签名 `new(player_name, machine_id, vendor, custom_protocol_keys, relay_nodes)` |
| `_managedResources.Add(guest/tcpClient)` | `self.managed_guests.lock().await.push(guest.clone())` | connect 成功后才入列表；TcpClient 由 Guest 内部管理（见决策 2） |
| `LogInformation("成功加入房间")` | `log::info!("成功加入房间")` | 一致 |
| `CloseAsync(ct)`：遍历资源按类型 Close/Leave/Dispose，最后 Clear | `close_all(ct)`：`std::mem::take` 清空两列表 → 逐个 `center.close(ct.clone())` / `guest.leave()` | 见决策 1、5、6 |
| `Dispose()` | 无（Rust Drop 自动回收） | 见 UNMAPPED |

## 偏差 / 决策记录

1. **托管列表按类型拆分**：C# 单一 `_managedResources` 列表靠运行时类型分发（`is ScaffoldingCenter` / `is ScaffoldingGuest`，其余 `Dispose`）；Rust 拆为 `managed_centers` / `managed_guests` 两个 Vec，编译期分派，结构更清晰且对齐契约字段约束。
2. **EasyTierManager / TcpClient 不单独托管**：C# 中 easyTier / discovery / tcpClient 是手动 new 后注册 Dispose；Rust 侧这些资源由 `ScaffoldingCenter` / `ScaffoldingGuest` 内部持有并自动回收，客户端无需也无权访问。
3. **`RelayNodeProvider.FetchAsync(ct)` 取消不传播**：Rust 侧 `provider.fetch()` 无取消参数（内部 10s 超时），对齐 relay/provider.rs 实际签名；C# ct 语义未移植。
4. **start/connect 成功后才入托管列表**：C# 先 `_managedResources.Add` 再 `StartAsync`（start 抛异常时资源仍留在列表，Dispose 时兜底清理）；Rust 契约明确 `start(ct).await?` 成功后才 push，失败直接返回，列表不含未启动的 center。行为更干净（轻微偏差）。
5. **`close_all` 用 `std::mem::take` 先清空再逐个关闭**：C# 语义为"遍历中关闭，遍历完 Clear"；Rust 因锁纪律不允许跨 `await` 持锁，先 take 出列表（等价"关闭后列表必为空"，且单个关闭失败不影响其余项）。C# 首个失败会中断并跳过剩余资源，Rust 改为记录 `log::error!` 后继续关闭其余（对齐契约返回 `()` 的设计）。
6. **`guest.leave()` 无取消参数（契约不一致，以实际签名优先）**：任务契约写 `leave(&self, ct)`，但 `scaffolding_guest.rs` 实际签名是 `pub async fn leave(&self)`（无 ct）。按"已存在则按实际签名"约束使用 `guest.leave()`，ct 不传入。若主控要求对齐契约，需同步修改 scaffolding_guest.rs。
7. **`close_all` 中 close 错误仅记日志**：契约 `close_all` 返回 `()`，`center.close(ct)` 的 `Result` 错误无法向上传播，记录 `log::error!` 后继续。

## 契约不一致风险（并行 subagent 协调）

- `ScaffoldingCenter` **尚未创建**（并行 subagent 进行中），本文件按任务契约使用：
  - `pub fn new(room_code: RoomCode, player_name: String, machine_id: String, vendor: String, minecraft_port: u16, relay_nodes: Option<Vec<String>>) -> Self`
  - `pub async fn start(&self, ct: CancellationToken) -> Result<(), ScaffoldingError>`
  - `pub async fn close(&self, ct: CancellationToken) -> Result<(), ScaffoldingError>`
  - `pub fn room_code(&self) -> &RoomCode`
  - `new` 第 6 参类型 `Option<Vec<String>>` 为推断值（调用处传 `Some(relay_nodes)`）；若实际为 `Vec<String>`，主控需协调修正本文件调用点。
- `guest.leave` 契约签名与实际签名不一致（见决策 6）。
- `relay::nodes::resolve` 实参类型为 `Option<&[String]>`，调用处以 `self.additional_relay_nodes.as_deref()` / `Some(&fetched)` 传入，与 nodes.rs 实际签名一致。
- `RelayNodeProvider::fetch()` 实际无取消参数（见决策 3）。

## 未覆盖（UNMAPPED 汇总）

- 构造函数参数 `easyTierPath`（Rust EasyTier 为库模式，无需路径）
- 构造函数参数 `loggerFactory` / `ILogger<T>` 结构化日志参数（Rust 用 log crate 文本日志替代）
- `CreateRoomAsync` 的 `customProtocols: IEnumerable<IProtocol>` 参数（契约未要求；Rust 侧 `ScaffoldingCenter` 无对应入参）
- C# `Dispose()`（Rust 无生命周期资源需手动释放，Arcs 随结构体 drop 自动回收）
- `FetchAsync(ct)` 取消语义（见决策 3）
- `_managedResources` 中非 center/guest 资源（EasyTierManager / TcpClient / CenterDiscoveryService）的注册与 Dispose（见决策 2）
- C# "start 失败后资源仍留列表兜底清理"语义（见决策 4）
