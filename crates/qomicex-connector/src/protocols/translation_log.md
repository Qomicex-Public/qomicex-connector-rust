# 翻译日志：Protocols 层（C# → Rust）

- 源文件：`scaffolding-src/Qomicex.Connector/Protocols/IProtocol.cs`、`scaffolding-src/Qomicex.Connector.Tests/Protocols/ProtocolHandlersTests.cs`
- 目标文件：`crates/qomicex-connector/src/protocols/mod.rs`
- 映射表条目：MAPPING_TABLE.yaml「Protocols/IProtocol.cs → src/protocols/mod.rs」（模块级行已存在，未修改）

## 决策记录

| # | 决策 | 理由 |
|---|------|------|
| D1 | trait 采用 `fn handle<'a>(&'a self, &'a ProtocolRequest) -> Pin<Box<dyn Future + Send + 'a>>`，不用 async_trait | 项目无 async_trait 依赖；标准协议全为同步计算 |
| D2 | 所有 `handle` 统一 `Box::pin(async move { ... })` | 保持简单一致，trait 签名统一 |
| D3 | PlayerPingProtocol 用 `serde_json::Value` 手动取字段 | 对齐 C# `JsonDocument` 语义：`GetString() ?? ""`、缺失属性容错；`PlayerProfileEntry` 无 `#[serde(default)]`，无需改模型 |
| D4 | JSON 解析/序列化失败 → status 255 + UTF8 错误消息 | C# 抛异常传播，Rust 无异常通道；与"未知协议 → status 255"约定一致 |
| D5 | `DelegateProtocol<TReq, TResp>` → `new_json_req`，空 body → `TReq::default()` | 对齐 C# `default!`；需 `TReq: DeserializeOwned + Default` |
| D6 | `PlayerKind` → `"HOST"/"GUEST"` 手动映射为 `Option<String>` | `PlayerKind` 无 serde derive；与现有 `PlayerProfileEntry.kind: Option<String>` 模型一致 |
| D7 | 测试用 `#[tokio::test]` | 需要 lib `Cargo.toml` 的 `[dev-dependencies]` 添加 tokio（见下） |

## 需要主控合并

1. `crates/qomicex-connector/src/lib.rs` 添加 `pub mod protocols;`
2. `crates/qomicex-connector/Cargo.toml` 添加 `[dev-dependencies]`：`tokio.workspace = true`（workspace tokio 已含 `macros` feature，仅缺 dev-dep 声明）

## UNMAPPED / 差异

- `CancellationToken` → 无对应（Rust trait 无取消参数；异步取消由调用方 drop future 实现）
- `JsonSerializerOptions`（C# 泛型 DelegateProtocol 可选参数）→ 忽略（serde 无 AOT/JsonSerializerContext 概念）
- `IReadOnlyList<string>` → `Vec<String>`
- C# 异常传播（`JsonException`/`KeyNotFoundException`）→ 统一 status 255 响应
- 根 JSON 非对象（如数组）时：C# `GetProperty` 抛异常，Rust 容错返回空字段（边缘差异）
- C# `DelegateProtocol<TResp>` / `<TReq, TResp>` 两个泛型类 → `new_json` / `new_json_req` 两个方法（无入参版不要求 `Default`）
- PlayerPingProtocol 的 `kind` 字段：C# 未赋值取枚举默认值 Host → Rust 显式 `PlayerKind::Host`
