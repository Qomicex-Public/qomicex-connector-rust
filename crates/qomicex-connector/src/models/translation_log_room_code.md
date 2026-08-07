# RoomCode 移植日志（C# → Rust）

源：`Qomicex.Connector/Models/RoomCode.cs` + `RoomCodeTests.cs`
目标：`crates/qomicex-connector/src/models/room_code.rs`
日期：2026-08-07

## 映射决策

| C# | Rust | 说明 |
|----|------|------|
| `class RoomCode`（public 属性） | `pub struct RoomCode`（私有字段 + pub 访问器） | 字段私有，访问器 `raw()` / `network_name_part()` / `secret_part()` / `easy_tier_network_secret()` 返回 `&str`；`easy_tier_network_name()` 返回 `String`（对应 C# 计算属性）。构造保持私有（`fn new`）。 |
| `Raw` / `NetworkNamePart` / `SecretPart` | 私有字段 + 访问器 | 同上 |
| `EasyTierNetworkName => $"scaffolding-mc-{NetworkNamePart}"` | `format!("scaffolding-mc-{}", self.network_name_part)` | 逐字 |
| `EasyTierNetworkSecret => SecretPart` | 返回 `&self.secret_part` | 逐字 |
| `Prefix = "U/"`、`Chars = "0123456789ABCDEFGHJKLMNPQRSTUVWXYZ"` | `const PREFIX` / `const CHARS` | 34 字符，无 I/O，逐字 |
| `Pattern`（正则） | 同上，用 `regex` crate；`OnceLock<Regex>` 惰性编译 | `regex.workspace = true` 已存在。C# verbatim string 转 Rust raw string，语义一致（`^...$` 均只匹配文本末尾）。 |
| `Parse` 抛 `RoomCodeInvalidException` | `parse() -> Result<Self, RoomCodeInvalidError>`，`Err(RoomCodeInvalidError(format!("无效的房间码格式: {code}")))` | 错误消息逐字保留 C# 文案。 |
| `Generate`（`RandomNumberGenerator.Fill`） | `rand::thread_rng().fill(&mut buffer)` | 新增依赖 `rand`（见下）。do-while 循环改为 `loop { ... if validate_checksum { break } }`，语义一致。 |
| `Encode8`（`Chars[bytes[i] % Chars.Length]`） | `encode8(&[u8; 8])`：`CHARS.as_bytes()[idx] as char` | 逐字 |
| `ValidateChecksum`（long 交替加减，`% 7 == 0`） | `i64` 交替加减，`% 7 == 0` | Rust 与 C# 整数取模均向零截断，`value % 7 == 0` 判等一致，直接照搬。 |
| `Generate` 内部 `return Parse(raw)` | `Self::parse(&raw).expect("生成的房间码必须合法")` | C# 此处必然成功；Rust 用 expect 表达不可达失败。 |

## 测试移植

| C# 测试 | Rust 测试 |
|---------|-----------|
| `Parse_ValidCode_ReturnsRoomCode` | `parse_valid_code_returns_room_code`（`assert_eq!`，含 EasyTier 名称/密钥断言） |
| `Parse_InvalidFormat_Throws` | `parse_invalid_format_returns_error`（`assert!(...is_err())`，测试数据 "bad" / "U/12" 一致） |
| `Generate_ProducesValidCode`（100 次循环重解析） | `generate_produces_valid_code`（`rand` 系统 RNG，无需种子，与 C# 一致） |

## 新增依赖（主控需在 Cargo.toml 添加）

`rand = { version = "0.8", features = ["std_rng"] }`

建议按现有模式添加：
- 根 `Cargo.toml` 的 `[workspace.dependencies]`：`rand = { version = "0.8", features = ["std_rng"] }`
- `crates/qomicex-connector/Cargo.toml` 的 `[dependencies]`：`rand.workspace = true`

代码中用法：`use rand::Rng;` + `rand::thread_rng().fill(&mut buffer)`（依赖 `Rng::fill`，0.8 标准 API）。

## UNMAPPED 项

1. `crate::error::RoomCodeInvalidError` —— 由另一 subagent 并行创建的模块，本文件按约定 `use crate::error::RoomCodeInvalidError;`，并按**元组结构体**（`RoomCodeInvalidError(String)`，thiserror `#[error("无效的房间码格式: {0}")]`）假设其构造方式。若实际为枚举/字段结构体，需主控调整构造调用（`room_code.rs` 中 `parse()` 内唯一使用处）。
2. `crate::error` 模块在 `lib.rs` 的注册（`pub mod error;`）由主控统一处理。

## 其他说明

- 未修改 MAPPING_TABLE.yaml；未执行 cargo/git；未写测试执行代码。
- 正则中的 `/` 在 C# 与 Rust 中均为字面量正斜杠，无需转义，语义一致。
