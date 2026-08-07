# 翻译日志：Models + 异常体系（C# → Rust）

源：`Qomicex.Connector/Models/*.cs` + `ScaffoldingException.cs`
目标：`crates/qomicex-connector/src/{models/*,error.rs}`
依据：MAPPING_TABLE.yaml 第 26-31 行

## 文件映射

| C# 源文件 | Rust 目标文件 | 状态 |
|---|---|---|
| Models/ProtocolFrame.cs | models/protocol.rs | ✅ 已翻译 |
| Models/PlayerInfo.cs + PlayerProfileEntry.cs | models/player.rs | ✅ 已翻译（合并，映射表第 27-28 行同文件） |
| Models/EasyTierNode.cs | models/easy_tier_node.rs | ✅ 已翻译 |
| Models/NetworkConfig.cs | models/network_config.rs | ✅ 已翻译 |
| ScaffoldingException.cs | error.rs | ✅ 已翻译 |
| lib.rs（模块声明） | lib.rs | ✅ 追加 `pub mod error;` |

## 逐项映射决策

### protocol.rs
- `ProtocolRequest.Namespace/RequestType/Body` → `namespace: String, request_type: String, body: Vec<u8>`；C# `""` 初值 → Rust Default derive（`String::new`）。
- `ProtocolResponse.Status`（`byte`，默认 0）→ `status: u8`；`IsSuccess` 属性 → `pub fn is_success(&self) -> bool { self.status == 0 }`。
- 两者均 `#[derive(Debug, Clone, Default, PartialEq, Eq)]`（约束 1）。

### player.rs
- `PlayerKind` 枚举：`#[derive(Debug, Clone, Copy, PartialEq, Eq)]`（约束 2，不 derive Default），另手写 `impl Default = Host`（C# 枚举默认值为首个成员 Host），使 `PlayerInfo` 可 derive Default 且语义与 C# 一致。
- `PlayerInfo`：`name/machine_id/vendor: String`（C# `""` 初值）、`easytier_id: Option<String>`（C# `string?` 可空）、`kind: PlayerKind`。derive `Debug, Clone, Default, PartialEq, Eq`。
- `PlayerProfileEntry`：`record` + `[JsonPropertyName("snake_case")]` → struct + `#[serde(rename_all = "snake_case")]`（约束 2）。
  - **不加** `#[serde(skip_serializing_if = "Option::is_none")]`：C# `System.Text.Json` 默认序列化 null（Guest 无 easytier_id 时带 null），保持行为一致。
  - `kind: Option<String>` 保持字符串序列化（"HOST"/"GUEST"），与 C# `string? Kind` 一致；映射表第 53 行确认 kind="HOST"/"GUEST"。
  - derive `Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize`（serde workspace 已开 derive feature）。

### easy_tier_node.rs
- `VirtualIp/Hostname/NodeId` → `virtual_ip/hostname/node_id: String`，derive `Debug, Clone, Default, PartialEq, Eq`（约束 3）。字段 snake_case 化。

### network_config.rs（重点：默认值）
- 字段映射：`NetworkName→network_name`、`NetworkSecret→network_secret`、`Hostname→hostname`、`NoTun→no_tun`、`UseSmoltcp→use_smoltcp`、`Dhcp→dhcp`、`Ipv4→ipv4: Option<String>`、`ListenRandomPorts→listen_random_ports`、`TcpWhitelist→tcp_whitelist: Vec<String>`、`PortForwards→port_forwards: Vec<String>`、`RelayNodes→relay_nodes: Option<Vec<String>>`（C# `string[]?` → `Option<Vec<String>>`）。
- **默认值**：`no_tun=true`、`dhcp=true`、`listen_random_ports=true`、`use_smoltcp=false`、`ipv4=None`、`relay_nodes=None`、列表为空。Rust `#[derive(Default)]` 会给 bool 全 false，故手写 `impl Default` 保持 C# 初值（约束 4）。
- derive `Debug, Clone, PartialEq, Eq`（不 derive Default）。

### error.rs
- C# 异常层级（`ScaffoldingException` 基类 + 7 个子类）→ 单一 `enum ScaffoldingError`，7 个变体各携带 `String` 消息（约束 5；Rust 无异常层级，枚举为惯用映射，与映射表第 31 行一致）。
- `Display` 直接输出携带消息（与 C# `Exception.Message` 语义一致，不附加前缀，避免双前缀）。
- `impl std::error::Error for ScaffoldingError {}`；`#[derive(Debug, Clone, PartialEq, Eq)]`。
- 变体文档注明各消息中文文本，与 README.md 第 277-282 行及调用处（RoomCode.cs:29、EasyTierManager.cs:109、CenterDiscoveryService.cs:36、ScaffoldingGuest.cs:112 等）推断一致。
- 未提供构造函数简写（约束 5 标可选；调用处消息各不相同，直接构造枚举与简写等价，故省略）。

## 有意省略（非 UNMAPPED，依据 MAPPING_TABLE）
- `NetworkConfig.EasyTierPath`：映射表（第 30 行）与任务约束 4 均未列出。原因：Rust 移植为 **easytier crate 库依赖（非进程）**（ADR D1、lib.rs 文档），不存在可执行文件路径概念，故字段整体省略。若后续保留进程模式可回填为 `Option<PathBuf>`。
- C# `Exception.InnerException`（两个构造函数）：Rust `thiserror` 枚举无内嵌异常机制，`#[source]` 字段场景在本次范围（纯数据模型）不适用，且映射表未要求；后续调用模块如需可加 `#[from]` 转换。

## UNMAPPED 项
无。

## 备注
- lib.rs 追加 `pub mod error;`（模块接线，MAPPING_TABLE 第 31 行指定 error.rs 位于 src/ 顶层；未触碰 models/mod.rs——5 个 model 模块已预先声明）。
- 未执行 cargo/git/测试（约束 7）。serde/thiserror 均为 workspace 既有依赖（Cargo.toml 已含）。
