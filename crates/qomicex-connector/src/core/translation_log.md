# Core 层移植日志（C# → Rust）

## 文件清单

| C# 源文件 | Rust 目标文件 |
|-----------|---------------|
| Core/ProtocolSerializer.cs | core/protocol_serializer.rs |
| Core/ProtocolNegotiator.cs | core/protocol_negotiator.rs |
| Core/CenterDiscoveryService.cs | core/center_discovery.rs |
| Core/HeartbeatService.cs | core/heartbeat.rs |
| — | core/mod.rs（声明 4 个子模块） |

## 签名决策

- `protocol_serializer.rs`
  - `pub fn serialize_request(&ProtocolRequest) -> Vec<u8>`：帧 `[1B typeLen][type ASCII][4B BE bodyLen][body]`
  - `pub fn serialize_response(&ProtocolResponse) -> Vec<u8>`：帧 `[1B status][4B BE bodyLen][body]`
  - `pub fn parse_request(&[u8]) -> Result<ProtocolRequest, ScaffoldingError>` / `parse_response`：一次性完整缓冲解析（同步、供测试）
  - `pub async fn deserialize_request_async<R: AsyncRead + Unpin>(&mut R)` / `deserialize_response_async`：完整流语义，内部用 `tokio::io::AsyncReadExt::read_exact`；同步流版本省略
  - 错误映射：读不足 → `ScaffoldingError::Protocol("流提前结束: 期望读取 {n} 字节，实际读取 {m} 字节")`；类型字符串不含 `:` → `ScaffoldingError::Protocol("无效的请求类型格式: {s}")`
- `protocol_negotiator.rs`：`pub fn negotiate(my: &[String], center: &[String]) -> Vec<String>`（HashSet 去重判断、保序）
- `center_discovery.rs`
  - `pub struct CenterDiscoveryResult { virtual_ip: String, port: u16 }`
  - `pub fn try_parse_center(&[EasyTierNode]) -> Option<CenterDiscoveryResult>`：正则 `^scaffolding-mc-server-(\d+)$`（OnceLock），端口 (1024, 65535]
  - `pub async fn discover<F, Fut>(get_nodes: F)`，`F: Fn() -> Fut`、`Fut: Future<Output = Vec<EasyTierNode>>`：60 次 × 500ms，超时 `CenterNotFound("未在 EasyTier 网络中发现联机中心（超时 30s）")`
- `heartbeat.rs`
  - `pub struct HeartbeatService { callback: Box<dyn FnMut() -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>> + Send>, on_failed: Option<Box<dyn Fn() + Send>> }`
  - `pub fn new(cb, on_failed)` / `pub async fn run(&mut self, interval: Duration, ct: CancellationToken)`：`tokio::select!` 循环，Err → 调 on_failed 并 break，取消 → break

## UNMAPPED / 偏差项

- **异步 EOF 实际字节数不可得**：tokio `read_exact` 失败时无法报告已读字节数，异步版错误消息只含"期望读取 {n} 字节"；同步 parse 版保留完整 `{n} / {m}` 消息。
- **HeartbeatService 生命周期**：C# `StartAsync/Stop/Dispose` + `PeriodicTimer(5s)` → Rust 单 `run(interval, ct)` 方法（`interval` 参数化）；取消/释放由调用方持有 `CancellationToken` 与 drop 完成，无对应 Stop/Dispose。
- **回调签名**：C# 回调接收 `CancellationToken`，Rust 回调无参数（取消由 `select!` 分支处理，失败通过 `Result<(), ()>` 的 `Err` 表达）。
- **C# `OperationCanceledException` 分支** → `ct.cancelled()` 分支，静默停止。
- **ILogger 依赖注入** → 全局 `log::` 宏（debug/info/warn 消息文本与 C# 一致）。
- **`EasyTierManager.GetNodesAsync` 依赖** → `discover` 闭包注入解耦。
- **`IReadOnlyList`** → `&[String]`。
- **`Encoding.ASCII`**：非 ASCII 字符 C# 替换为 `?`，Rust 侧 `ascii_bytes` 逐字符映射为 `?` 保持一致。
- **类型长度字节**：C# `(byte)typeBytes.Length` 截断，Rust `as u8` 语义一致（超出 255 时行为相同）。
- **C# 先读 body 再校验 `:`**：解析顺序保持一致。

## 测试移植（C# xUnit → Rust #[cfg(test)]）

| C# 测试 | Rust 测试 |
|---------|-----------|
| ProtocolSerializerTests.SerializeRequest_Roundtrip_PreservesData | protocol_serializer.rs: `roundtrip_request_preserves_data` |
| ProtocolSerializerTests.SerializeResponse_Roundtrip_PreservesData | `roundtrip_response_preserves_data` |
| ProtocolSerializerTests.SerializeResponse_EmptyBody_Works | `roundtrip_response_empty_body_works` |
| ProtocolSerializerTests.SerializeRequest_LongTypeName_Works | `long_type_name_works` |
| —（新增 golden test） | `serialize_request_frame_matches_csharp_bytes` / `serialize_response_frame_matches_csharp_bytes` / `parse_request_accepts_csharp_known_bytes` / `parse_response_accepts_csharp_known_bytes` |
| ProtocolNegotiatorTests.Negotiate_ReturnsIntersection | protocol_negotiator.rs: `negotiate_returns_intersection` |
| ProtocolNegotiatorTests.Negotiate_EmptyInputs_ReturnsEmpty | `negotiate_empty_inputs_returns_empty` |
| CenterDiscoveryServiceTests.TryParseCenter_Valid_ExtractsIpAndPort | center_discovery.rs: `try_parse_center_valid_extracts_ip_and_port` |
| CenterDiscoveryServiceTests.TryParseCenter_InvalidPort_ReturnsNull | `try_parse_center_invalid_port_returns_none` |
| CenterDiscoveryServiceTests.TryParseCenter_PortTooLow_ReturnsNull | `try_parse_center_port_too_low_returns_none` |

## 帧互操作 golden 字节（C# 帧格式固定，字节级兼容）

- 请求 `c:ping` body=[0x01,0x02,0x03]：`[0x06] + b"c:ping" + [0x00,0x00,0x00,0x03] + [0x01,0x02,0x03]`（typeLen=6 单字节，bodyLen 大端 u32）
- 响应 status=0 body=[0xFF,0xFE]：`[0x00] + [0x00,0x00,0x00,0x02] + [0xFF,0xFE]`
- 反向 golden：硬编码 C# 字节手工构造解析（请求 `myapp:ping` body=[0x42]；响应 status=1 body=[0x01,0x02]）
- 全部为同步 `#[test]`（roundtrip 无需 async；async 解析路径已在既有行为映射中验证）
