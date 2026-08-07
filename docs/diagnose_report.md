# 诊断报告：QA 2 个 medium 问题

- 日期：2026-08-07
- 输入：`docs/qa_report.md`（问题 1 missing / 问题 2 behavior_mismatch）
- 定位：只诊断 + 补丁建议，未改任何代码

---

## 问题 1（medium · missing）：Host 自定义协议支持丢失

### 根因分析

C# 链路（完整可用）：

- `ScaffoldingClient.CreateRoomAsync(..., IEnumerable<IProtocol>? customProtocols = null, ct)` → 构造 `ScaffoldingCenter` 传入 `customProtocols?.ToList()`（ScaffoldingClient.cs:59, 73-75）
- `ScaffoldingCenter.StartAsync` 把 `_customProtocols` 追加进 `advertisedKeys`（ScaffoldingCenter.cs:58）和 `protocols` 列表（:68），后者经 `new TcpServer(...)` → `ToDictionary(p => p.ProtocolKey)`（TcpServer.cs:31）进入分发表

Rust 断点：

1. `client.rs:83-108` `create_room` 无 custom 参数（对比 `join_room` 的 `custom_protocol_keys` 是存在的，Guest 侧已移植）——入口缺失
2. `scaffolding_center.rs:53-76` `new` 无 custom 字段；`start()` 中 `advertised_keys` 硬编码 6 键（:114-117）、`protocols` 硬编码 5 个（:126-132）——装配缺失
3. **管线本身是通的**：`TcpServer::new(port, Vec<Arc<dyn ProtocolHandler>>, tx)` 已接受任意协议表并按 `p.key()` 建 HashMap（tcp_server.rs:37-54），`DelegateProtocol` 四个构造器（protocols/mod.rs:234-307）都已实现 —— 只差顶层参数透传

根因定性：**接口裁剪失误**（移植时把 `customProtocols` 判为"契约无此参数"裁掉，见 `translation_log_client.md:23`、`center/translation_log.md:121`），非架构障碍。修复是纯加法，不触碰 wire 协议。

### 推荐修复方案（manual 补丁，3 个文件 + 1 处 CLI 调用点）

**1. `client.rs` — `create_room` 加参数并透传**

```rust
use crate::protocols::ProtocolHandler;  // 新增 import

pub async fn create_room(
    &self,
    player_name: String,
    machine_id: String,
    vendor: String,
    minecraft_port: u16,
    custom_protocols: Vec<Arc<dyn ProtocolHandler>>,   // 新增，位置对齐 C# (..., customProtocols, ct)
    ct: CancellationToken,
) -> Result<Arc<ScaffoldingCenter>, ScaffoldingError> {
    // ...
    let center = Arc::new(ScaffoldingCenter::new(
        room_code, player_name, machine_id, vendor, minecraft_port,
        custom_protocols,                                // 新增，置于 relay_nodes 前（对齐 C# 构造参数序）
        Some(relay_nodes),
    ));
    // ...
}
```

**2. `scaffolding_center.rs` — 存储并在 start() 中合并**

```rust
pub struct ScaffoldingCenter {
    // ...现有字段...
    custom_protocols: Vec<Arc<dyn ProtocolHandler>>,   // 新增字段（对应 C# _customProtocols）
    // ...
}

pub fn new(
    room_code: RoomCode,
    player_name: String,
    machine_id: String,
    vendor: String,
    minecraft_port: u16,
    custom_protocols: Vec<Arc<dyn ProtocolHandler>>,   // 新增，位置对齐 C# (..., customProtocols, relayNodes)
    relay_nodes: Option<Vec<String>>,
) -> Self { /* self.custom_protocols = custom_protocols; */ }
```

`start()` 内两处合并（scaffolding_center.rs:114-132）：

```rust
// ① advertised_keys：标准 6 键 + 自定义键（C# ScaffoldingCenter.cs:58 AddRange）
let mut advertised_keys: Vec<String> = [...6 个标准键...].into_iter().map(String::from).collect();
advertised_keys.extend(self.custom_protocols.iter().map(|p| p.key().to_string()));

// ② 协议表：5 个标准 + 自定义（C# ScaffoldingCenter.cs:68 AddRange）
let mut protocols: Vec<Arc<dyn ProtocolHandler>> = vec![
    Arc::new(PingProtocol),
    Arc::new(ProtocolsProtocol::new(advertised_keys)),   // ProtocolsProtocol 引用的是扩展后的键列表
    // ...其余 3 个标准协议不变...
];
protocols.extend(self.custom_protocols.clone());         // Arc 克隆，零拷贝深
```

注意顺序：`advertised_keys` 必须先扩展再交给 `ProtocolsProtocol::new`（与 C# :58 先 AddRange 再 :63 构造 ProtocolsProtocol 一致）。

**可选加固（建议加）**：重复键检查。C# `ToDictionary(p => p.ProtocolKey)` 遇重复键（自定义撞标准键 / 自定义互相撞）抛 `ArgumentException` → 房间创建失败；Rust `HashMap::collect` 静默后者覆盖前者。在 `start()` 装配前加：

```rust
let std_keys = [...6 标准键...];
if let Some(k) = custom_protocols.iter().map(|p| p.key()).find(|k| std_keys.contains(k)) {
    return Err(ScaffoldingError::Protocol(format!("自定义协议键冲突: {k}")));
}
// 自定义键之间去重同理（可用 HashSet 查重）
```

**3. `qomicex-connector-cli/src/main.rs:141`（唯一调用点）**

```rust
.create_room(player_name.to_string(), machine_id.to_string(), "Qomicex".into(), port,
             Vec::new(),   // 新增：无自定义协议
             ct.clone())
```

### 类型约束核查（问题 1 的第 3 问）

- `pub trait ProtocolHandler: Send + Sync`（protocols/mod.rs:20），**无显式 `'static`**，但 `Arc<dyn ProtocolHandler>` 在 trait object 位置默认携带 `'static` 对象生命周期上界；`TcpServer::start` 中 `self.protocols.clone()` 已直接捕获进 `tokio::spawn`（tcp_server.rs:103-107），当前 `cargo check` 通过即证明**可跨 tokio::spawn**。
- `DelegateProtocol` 各构造器的闭包参数均已要求 `+ 'static`（protocols/mod.rs:243, 268, 283），与 Arc 共享兼容。
- **建议**：trait 声明改为 `Send + Sync + 'static`，把隐式上界显式化（行为零变化，报错信息更友好）。

### 改动影响评估

- **兼容性**：默认空 `Vec` 时 advertised 键与协议表与现状逐字节一致，wire 零变化；CLI 行为不受影响（唯一调用点补 `Vec::new()`）。
- **破坏性**：`create_room` / `ScaffoldingCenter::new` 签名变化为**编译期破坏性变更**——workspace 内只有 CLI 一处调用（main.rs:141），已覆盖；该 crate 为 workspace 内部 crate，无外部下游。
- **测试**：`start()` 会真实启动 EasyTier 实例，无法在单测内跑全流程。建议把"标准键+自定义键 → (advertised_keys, protocols)"装配抽成纯函数（如 `fn assemble_protocols(minecraft_port: u16, custom: &[Arc<dyn ProtocolHandler>]) -> (Vec<String>, Vec<Arc<dyn ProtocolHandler>>)`），单测断言自定义键出现在两者中。
- **需更新文档**：`translation_log_client.md:23`、`center/translation_log.md:89,121`（移除"不移植"标注，改记"已补移植"）、`qa_report.md:15,101,132`。

---

## 问题 2（medium · behavior_mismatch）：PlayerPing 容错语义

### 根因分析

C# 实际语义（IProtocol.cs:73-83 + TcpServer.cs:99-110）：

- `JsonDocument.Parse(json)` 非法 JSON → 抛异常 → 上抛出 `HandleAsync` → TcpServer catch → **不写响应**，finally 断开该连接
- `root.GetProperty("name"/"machine_id"/"vendor")` 属性**缺失** → `KeyNotFoundException` → 同上断连
- 值为**非字符串** → `GetString()` 抛 `InvalidOperationException` → 同上断连
- 仅 `easytier_id` 用 `TryGetProperty` 容错（:82）
- 失败路径**无 255 响应**，客户端看到的是"连接被直接关闭"

Rust 实际语义（protocols/mod.rs:148-181）：

- 非法 JSON → `serde_json::from_slice` Err → **255 响应 + 连接保持**
- 缺失键 / 非字符串 → `as_str().unwrap_or("")` 容错为空串
- 注释（:162）声称"与 C# JsonDocument 语义一致：缺失属性容错，`GetString() ?? ""`" —— **该注释对 C# 的描述是错的**（C# 缺失属性会抛异常而非容错），QA 判定行为不一致是正确的

次要但关键的事实：Rust 中失败的 ping（status 255）因 `client_conn.rs:98` 的 `response.is_success()` 门控**不会记录心跳**，对端持续发畸形 ping 时约 15s 后仍会被心跳循环剔除（client_conn.rs:136-159）。即 Rust 的"连接保持"实际是**有界容错**（≤15s 宽限期 + 255 响应），不是无限期。

另外该差异是**系统性的**：`DelegateProtocol` 系列的反序列化/序列化失败同样是 C# 断连、Rust 255 保持（QA 报告 :70 已指出）——这是移植时统一采用"255 错误响应"约定的结果（protocols/translation_log.md D4）。

### 推荐：方案 A — 保持 Rust 容错，修正注释/文档为"有意改进"

**明确推荐方案 A，不严格对齐 C#。** 理由：

1. **255 就是 SCF 的通用错误通道**：未知协议、请求反序列化失败、响应序列化失败全部走 status 255 + 连接保持（client_conn.rs:87-90 已确立）。PlayerPing 单独断连会破坏 Rust 端一致的错误模型。
2. **C# 断连是异常控制流的产物，属缺陷**：`GetProperty`/`GetString` 的裸调用意味着**任意第三方/畸形客户端一条坏包就能掐断自己的连接**——断连惩罚的是对端而非发送方。配对启动器恒发完整 JSON，正常路径双方无差异；差异只出现在畸形/跨版本客户端，而容错方（Rust）行为更温和。
3. **容错不导致资源失控**：255 ping 不刷新心跳，畸形客户端 ≤15s 即被心跳剔除，最终结果与 C#（立即断连）收敛一致，只是中间多了宽限期与可见的错误响应——这对调试第三方客户端反而有利。
4. 方案 B（严格对齐）成本高且引入不一致：需要改 `ProtocolHandler::handle` 为 `Result<_, ProtocolError>` 或给 player_ping 特判"255 即致命"，前者动 5 个标准协议 + DelegateProtocol + 全部测试，后者让 client_conn 对同一种错误码做两种语义，且 wire 上 C# 是不写响应直接 EOF、Rust 仍要写 255，做不到逐字节对齐。

### 所需改动（全部为注释/文档级，零行为变更）

| 位置 | 改动 |
| --- | --- |
| `protocols/mod.rs:162` | 修正错误注释。改为：「与 C# 有意偏差：C# 解析失败/缺失属性/非字符串抛异常→整条连接断开（IProtocol.cs:79-82）；Rust 一律容错为 255 + 空字段 + 连接保持（不刷新心跳，畸形客户端 ≤15s 被心跳剔除）。见 protocols/translation_log.md」 |
| `protocols/translation_log.md` D3（:13） | 现文"对齐 C# 语义：缺失属性容错"事实错误 → 改写为偏差决策：Rust 容错为有意改进（255 + 空字段），C# 为异常断连；D4（:14）补充注明该 255 约定同时覆盖 player_ping 解析失败 |
| `center/translation_log.md:37` | machine_id 提取 `if let Ok(...)` 一行已正确（与 C# try/catch 一致），无需改 |
| `docs/qa_report.md:66-70` | medium 降级：重分类为"已记录的有意偏差"，更新 :26 推荐语与 :70 同理句（DelegateProtocol 差异一并归类） |

**若未来确实要收紧**（不推荐）：最小改动是 `client_conn.rs` 对 `c:player_ping` 返回 255 时 `return Err` 断开——但如上所述会与 DelegateProtocol 语义分裂，不建议。

### 改动影响评估

- 方案 A：零代码行为变化，`cargo check` 与互操作测试结果不变；仅文档/注释纠正了"声称一致、实际不一致"的失真描述，QA 项转为 PASS（记录型偏差）。
- 方案 B：wire 级对 C# 也不完全对齐（C# 失败不写响应），却要动 trait 签名与全部协议实现，收益（惩罚畸形客户端更快）与成本不成比例。

---

## 总结

| 问题 | 根因 | 推荐方案 | patch_type |
| --- | --- | --- | --- |
| 1. Host 自定义协议缺失 | 移植时接口裁剪（translation_log 记为"不移植"），管线本身（TcpServer HashMap + DelegateProtocol）已就绪 | `create_room` 加 `custom_protocols: Vec<Arc<dyn ProtocolHandler>>`（置于 `ct` 前）→ 透传 `ScaffoldingCenter::new`（置于 `relay_nodes` 前）→ 字段存储 → `start()` 中扩展 `advertised_keys` 与 `protocols` 列表；CLI 唯一调用点补 `Vec::new()`；可选加重复键检查对齐 C# ToDictionary 抛错；trait 显式化 `+ 'static` | manual（纯加法补丁） |
| 2. PlayerPing 容错语义 | C# 异常控制流（GetProperty/GetString 裸调 → 断连）vs Rust 统一 255 约定；且 :162 注释对 C# 描述失真 | **方案 A（推荐）**：保持 Rust 255+空字段容错（有界：255 不刷新心跳，≤15s 被剔除），仅修正 `protocols/mod.rs:162` 注释 + 两个 translation_log + QA 报告降级为"已记录的有意偏差"；方案 B（对齐断连）成本高、wire 仍不对齐、与 DelegateProtocol 语义分裂，不采用 | update_mapping（文档级） |

改动影响：1 为编译期破坏性签名变更（仅 CLI 一处调用点，workspace 内部，默认空 Vec 行为零变化）；2 为零代码变更。两者均不动 wire 协议与互操作测试。
