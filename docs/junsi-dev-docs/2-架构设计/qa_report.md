# QA 质检报告：Qomicex.Connector (C#) → qomicex-connector (Rust) 快照比对

- 日期：2026-08-07
- 源：`C:\Users\tmoam\AppData\Local\Temp\opencode\scaffolding-src\Qomicex.Connector\`
- 目标：`C:\Project\qomicex-connector-rust\crates\qomicex-connector\src\`
- 方式：代码级逐对行为核查（非逐行抄写）
- 编译验证：`cargo check -p qomicex-connector --lib` 通过（0.49s，无警告）

```json
{
  "status": "FAIL",
  "checked_modules": 13,
  "diff_count": 10,
  "diff_details": [
    { "module": "ScaffoldingClient/Center", "source": "CreateRoomAsync(customProtocols) 注册到 advertisedKeys+协议表", "target": "create_room/ScaffoldingCenter 无 custom 协议参数，硬编码 5 个标准协议", "type": "missing", "severity": "medium" },
    { "module": "Protocols/IProtocol", "source": "PlayerPing 缺属性/非字符串时 GetProperty/GetString 抛异常→连接断开", "target": "缺失键→空串容错，解析失败→255 响应保持连接", "type": "behavior_mismatch", "severity": "medium" },
    { "module": "CenterDiscoveryService", "source": "DiscoverAsync(ct) 取消传播，中断 60×500ms 扫描", "target": "discover() 无 ct，取消后被忽略直到 30s 结束", "type": "deviation", "severity": "low" },
    { "module": "ProtocolSerializer", "source": "ASCII.GetString 高位字节→'?' (0x3F)", "target": "from_utf8_lossy 非法字节→U+FFFD", "type": "deviation", "severity": "low" },
    { "module": "HeartbeatService", "source": "PeriodicTimer 固定 5s 节奏（无漂移）", "target": "sleep(interval) 回调结束后重置（累积漂移）", "type": "deviation", "severity": "low" },
    { "module": "TcpServer", "source": "心跳超时 ClientDisconnected 触发 2 次（machineId 后 null）", "target": "仅触发 1 次（取消令牌→连接任务清理）", "type": "deviation", "severity": "low" },
    { "module": "Guest/TcpClient", "source": "IsConnected 轮询 socket Connected 状态", "target": "is_connected = stream.is_some()（断线后过期）", "type": "deviation", "severity": "low" },
    { "module": "RelayNodeProvider", "source": "RegionInfo.CurrentRegion.TwoLetterISORegionName", "target": "sys_locale 取 '-' 后段；下划线 locale 回退 CN", "type": "deviation", "severity": "low" },
    { "module": "Guest/ScaffoldingGuest", "source": "MachineId[..8] 字符切片（安全）", "target": "machine_id[..8] 字节切片（非 ASCII 边界 panic）", "type": "deviation", "severity": "low" },
    { "module": "Guest/ScaffoldingGuest", "source": "server_port 响应体 2 字节外无校验（不足时抛异常）", "target": "额外长度校验返回错误（结果同为错误）", "type": "deviation", "severity": "low" }
  ],
  "recommendation": "修复 2 个 medium：① ScaffoldingCenter::start / create_room 补回 customProtocols 参数（advertised + 协议表注册）；② PlayerPing 解析对齐 C# 语义（缺属性/非字符串→错误断开）或文档化 255 容错为有意偏差。其余 low 项可选择接受（建议 1 项改为字节切片安全检查）。"
}
```

---

## 各模块比对明细

### 1. Models/RoomCode.cs ↔ models/room_code.rs — PASS
- 正则 `^U/([A-HJ-NP-Z0-9]{4})-...{4}$` 完全一致（room_code.rs:21）
- 字符表 `0123456789ABCDEFGHJKLMNPQRSTUVWXYZ`（base-34）一致
- 校验和：`i%2==0 加 / 奇数减`，`%7 == 0`（room_code.rs:112，C# RoomCode.cs:73）
- Generate：8 随机字节 `%34` 映射 8 字符、两组 do-while 校验、`U/{p1[..4]}-{p1[4..]}-{p2[..4]}-{p2[4..]}` 拼接（room_code.rs:75-91）完全一致
- Parse 错误消息文本一致（`无效的房间码格式: {code}`）

### 2. Core/ProtocolSerializer.cs ↔ core/protocol_serializer.rs — PASS（含 1 个 low 偏差）
- 请求帧 `[1B typeLen][type ASCII][4B BE len][body]`（protocol_serializer.rs:16-24）✓
- 响应帧 `[1B status][4B BE len][body]`（:27-33）✓
- ASCII 编码方向：C# `Encoding.ASCII.GetBytes` 非 ASCII→'?'，Rust `ascii_bytes` 同语义（:9-13）✓
- 无 ':' → `无效的请求类型格式` 错误一致（:52-62）
- EOF 处理：C# `ReadExact` 提前结束抛 `流提前结束: 期望读取 N 字节，实际读取 M`；Rust 同步/异步版同文本（:36-49, :100-106）✓
- 互操作测试用例直接以 C# 已知字节验证（:201-240）
- **low 偏差**：反序列化方向 C# `ASCII.GetString` 高位字节→`?`；Rust `from_utf8_lossy`→U+FFFD。仅影响含非 ASCII 字节的畸形 type 串，且两者都会命中 split_type_str 或正常 ASCII 路径，实际不可区分。

### 3. Core/ProtocolNegotiator.cs ↔ core/protocol_negotiator.rs — PASS
- 取交集且保持 my 顺序（HashSet 查重 + filter），完全一致（protocol_negotiator.rs:6-12）

### 4. Core/CenterDiscoveryService.cs ↔ core/center_discovery.rs — PASS（含 1 个 low 偏差）
- 正则 `^scaffolding-mc-server-(\d+)$` 一致（center_discovery.rs:17）
- 端口条件 `> 1024 && <= 65535` 一致（:39）
- 60 次 × 500ms 轮询、超时消息文本一致（:55-76）
- **low 偏差**：C# `DiscoverAsync(easyTier, ct)` 取消传播（`Task.Delay(500, ct)` 抛 OCE）；Rust `discover(get_nodes)` 无 ct 参数，取消后仍跑满 30s（center_discovery.rs:50，调用点 scaffolding_guest.rs:150-154 未传入 ct）。

### 5. Protocols/IProtocol.cs ↔ protocols/mod.rs — 2 项偏差（1 medium）
- Ping 回显 status 0 + 原样 body ✓（protocols/mod.rs:51-58）
- Protocols `join('\0')` ✓（:87）
- ServerPort 大端 u16 ✓（:118）
- PlayerProfilesList snake_case + kind "HOST"/"GUEST" ✓（:220-231；player.rs:38 `#[serde(rename_all="snake_case")]`，null 字段默认序列化，与 System.Text.Json 默认一致）
- 未知协议 → status 255 + UTF8 `未知协议: {key}` ✓（client_conn.rs:87-90，C# TcpServer.cs:79-80）
- DelegateProtocol 系列 → new_raw / new / new_json / new_json_req ✓（已知偏差：serde_json 替代泛型）
- **medium 行为偏差（PlayerPing 解析容错）**：
  - C#（IProtocol.cs:79-82）：`root.GetProperty("name").GetString() ?? ""` — 属性**缺失**时 `GetProperty` 抛 `KeyNotFoundException`，值为**非字符串**时 `GetString()` 抛异常 → 异常向上传播 → TcpServer 外层 catch → **连接被断开**，不写响应（TcpServer.cs:74-75, 99-102）。
  - Rust（protocols/mod.rs:163-181）：`root.get("name").and_then(as_str).unwrap_or("")` — 缺失/非字符串一律容错为 `""`；JSON 解析失败返回 255 响应，**连接保持**。
  - 影响：畸形或缺少字段的 player_ping，C# 表现为"断开该客户端"，Rust 表现为"空名玩家 + 255 响应"。配对客户端恒发完整 JSON，正常路径不受影响；但跨版本/第三方客户端行为不一致。
  - 同理：C# `DelegateProtocol<TReq,TResp>` 序列化异常 → 断开；Rust 返回 255 保持连接。

### 6. EasyTierManager.cs ↔ easytier/manager.rs — PASS（核心架构已知偏差 D1-D7）
- 参数映射逐项核对（build_toml_config，manager.rs:134-187）：
  - network_name/secret/hostname → `NetworkIdentity::new` + `set_hostname` ✓
  - ipv4 → `10.144.144.1` 自动拼 `/24`（C# 传纯 IP，easytier 需 CIDR；manager.rs:142-145）✓ 行为等价
  - dhcp 分支（ipv4 None 时）✓
  - listen_random_ports → `tcp://0.0.0.0:0` + `udp://0.0.0.0:0` ✓
  - `--compression=zstd --multi-thread --latency-first --enable-kcp-proxy` → Flags 字段 ✓
  - tcp_whitelist 恒含 `"0"` 前缀 + 追加端口；udp_whitelist `["0"]` ✓（对齐 C# 先 `--tcp-whitelist=0` 再追加）
  - peers = relay_nodes ?? 默认 ✓（C# `config.RelayNodes ?? RelayNodes.Default` ↔ Rust `relay_nodes.clone().unwrap_or_else(|| resolve(None,None))`）
  - port_forwards 解析 `tcp://127.0.0.1:LOCAL/REMOTE:PORT` → bind/dst/proto ✓
  - 启动等待：is_ready 轮询 ≤30s + 2s 缓冲 ✓（D4；C# 解析 stdout "listener added"）
  - 节点列表：route_snapshots → Route{peer_id, hostname, ipv4}，ipv4 去掩码 ✓（D5；对齐 C# CLI peer 输出 `IP 可能带 /24` 截断逻辑）
  - NetworkConfig 默认值（no_tun=true, use_smoltcp=false, dhcp=true, listen_random_ports=true）与 C# 一致（network_config.rs:30-46）

### 7. RelayNodeProvider.cs ↔ relay/provider.rs — PASS（含 1 个 low 偏差）
- ENDPOINT / DefaultUserAgent 常量一致（provider.rs:10-13）
- region 排序：preferred 匹配（大小写不敏感）前置、其余保序 ✓（:142-165；C# RelayNodeProvider.cs:124-146 同构）
- http(s) 节点二次解析（trim、失败跳过）✓（:104-116）
- 失败/空列表回退默认节点 ✓（:74-88）
- 10s HTTP 超时 ✓
- **low 偏差**：地区检测 — C# `RegionInfo.CurrentRegion.TwoLetterISORegionName`（如 "US"/"CN"）；Rust `sys_locale` 取 `-` 后段（"zh-CN"→"CN"；"en_US" 下划线格式 → 回退 "CN"）。hyphen 格式正常系统下等价，下划线系统下可能不同。

### 8. Center/ScaffoldingCenter.cs ↔ center/scaffolding_center.rs — 1 项 missing（medium）
- 端口扫描 1025..65535 试绑、全失败回退 25000 ✓（scaffolding_center.rs:102-112）
- advertised keys 6 个标准协议 ✓（:114-117）
- 玩家 Host 入列（kind=Host，easytier_id=NodeId）✓（:172-173）
- OnPlayerPing 更新（name/easytier_id/vendor，不更新 machine_id）/ 新增（kind=Guest）✓（:195-209）
- ClientDisconnected 移除非 Host ✓（:212-224）
- PlayersChanged 通知（watch channel，已知偏差）✓
- **medium missing（自定义协议）**：C# `ScaffoldingCenter` 构造 + `StartAsync` 把 `customProtocols` 追加进 advertisedKeys 和协议表（ScaffoldingCenter.cs:47, 53-68）；Rust `ScaffoldingCenter::new`（scaffolding_center.rs:53-60）与 `ScaffoldingClient::create_room`（client.rs:83-108）均无此参数，协议表硬编码 5 个标准协议。`DelegateProtocol` 基础设施存在（protocols/mod.rs:234-307）但 Host 侧不可达 —— 功能缺失。

### 9. Center/TcpServer.cs ↔ center/tcp_server.rs + client_conn.rs — PASS（含 2 个 low 偏差）
- 请求循环：读帧→查协议表→分发→写回；未知协议 255 ✓（client_conn.rs:67-119）
- 心跳 15s 超时、5s 检查周期 ✓（client_conn.rs:17-20, 136-160）
- machine_id 提取容错（player_ping 成功后 TryGetProperty + ?? ""）✓（client_conn.rs:98-114）
- 清理顺序：active/last_heartbeat/client_machine 移除 + 断开通知 ✓
- **low 偏差 1**：心跳超时 C# 触发 2 次 ClientDisconnected（超时循环里显式 invoke + 连接任务 finally 再 invoke，TcpServer.cs:124-135 vs 108）；Rust 仅连接任务清理时触发 1 次（client_conn.rs:126-129）。玩家移除结果一致，仅 C# 多一次空 machineId 通知（多一次 PlayersChanged）。
- **low 偏差 2**：C# 超时客户端直接 Close() socket；Rust 取消连接令牌由任务自行退出 —— 效果等价。

### 10. Guest/ScaffoldingGuest.cs ↔ guest/scaffolding_guest.rs — PASS（含 2 个 low 偏差）
- 直连（虚拟 IP，3s 超时）→ 失败 Stop+PortForwards+重启 → 转发重连 ✓（:159-201）
- 重试 10 次 × 2s 间隔、每次 3s 超时 ✓（:240-263）
- MapMinecraftPort 幂等（已映射直接返回）✓（:402-408）
- 协商协议列表（\0 join + 交集 + 保序）✓（:306-334）
- easytier_id 条件字段（协商含 c:player_easytier_id 才带，NodeId ?? "" → 空串）✓（:273-290）
- kind 字段心跳中为 null，与 C# PlayerProfileEntry 默认 null + STJ 默认序列化 null 一致 ✓
- GetServerPort：状态 32-63 抛"MC 服务器未启动"、非成功抛错、BE u16 ✓（:363-381）
- GetPlayerList：kind=="HOST" 判断、缺失字段空串 ✓（:499-533）
- ConnectionLost 一次性语义：C# `_connectionLostFired` 防重；Rust watch send(true) 值去重 → 等价 ✓
- **low 偏差 1**：C# `MachineId[..Math.Min(8, Length)]` 字符切片安全；Rust `&self.machine_id[..self.machine_id.len().min(8)]`（scaffolding_guest.rs:134）字节切片，machine_id 含非 ASCII（中文/emoji）时 panic。
- **low 偏差 2**：`get_server_port` Rust 增加 body 长度 != 2 校验（:374-381）；C# 短 body 时 `ReadUInt16BigEndian` 同样抛异常 —— 结果等价（错误），Rust 更明确。
- 取消传播：重试间 `Task.Delay(2s, ct)`（C#）vs `sleep` 无 ct（Rust）—— 同偏差类 4。

### 11. Guest/TcpClient.cs ↔ guest/tcp_client.rs — PASS（含 1 个 low 偏差）
- 15s 读超时（C# ReadTimeout=15000 → Rust timeout 包裹整个响应读取）✓（tcp_client.rs:85-89）
- 发送锁（SemaphoreSlim → tokio Mutex）写+读全程持有 ✓（:69-95）
- 未连接 → `CenterConnection("未连接到联机中心")` 同文本 ✓
- 发送失败/超时 → 日志 + 错误传播 ✓
- **low 偏差**：`is_connected` — C# `_client?.Connected == true` 轮询 socket 状态；Rust `stream.is_some()`（tcp_client.rs:106-108），对端断线后返回 true 直到下次 send 失败。代码注释已声明。

### 12. ScaffoldingClient.cs ↔ client.rs — 1 项 missing（medium，同 8）
- 中继缓存（override → resolve；否则 fetch → resolve；缓存复用）✓（client.rs:63-79）
- CreateRoom 流程：Generate → 解析中继 → Center start ✓（除 customProtocols）
- JoinRoom 流程：Parse → 解析中继 → Guest connect ✓（customProtocolKeys 支持 ✓）
- CloseAll：Center close / Guest leave / 清空 ✓（:141-153）
- **medium missing**：`create_room` 无 customProtocols 参数（C# ScaffoldingClient.cs:54-57 有）。

### 13. Models/PlayerProfileEntry 序列化 — PASS
- snake_case：name/machine_id/easytier_id/vendor/kind 一致（player.rs:38-49；C# JsonPropertyName + JsonContext SnakeCaseLower 双重确认）
- 空字符串与 null 语义一致（C# 默认序列化 null 属性 ↔ serde Option None → null）
- kind 字符串 "HOST"/"GUEST" 一致

---

## 已知有意偏差（不算 FAIL，记录存档）
- ILogger 注入 → log crate 全局（tcp_server.rs:33 注释、tcp_client.rs:29）
- CancellationToken → util.rs 自实现（AtomicBool + Notify）
- HeartbeatService 固定 5s → 参数化 interval（heartbeat.rs:32）
- event → tokio watch/mpsc channel（PlayersChanged、ConnectionLost、ClientDisconnected）
- DelegateProtocol 泛型 → serde_json 版本
- EasyTier 进程 → easytier 库（MAPPING_TABLE D1-D7；is_ready 替代 stdout 解析、route_snapshots 替代 cli peer、ipv4 /24 拼接）

## 结论
- 13 个模块中 11 个行为一致（含已知偏差项），2 个 medium 问题（Host 自定义协议缺失、PlayerPing 容错语义偏差），8 个 low 偏差。
- **无 wire-protocol 级不一致**（帧格式、255、大端序、snake_case、协商、心跳窗口均已字节级对齐并有互操作测试）。
- `cargo check -p qomicex-connector --lib` 通过。
- 建议优先处理 2 个 medium 后转 PASS。
