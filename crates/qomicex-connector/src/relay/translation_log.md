# 移植日志：中继节点模块（RelayNodes / RelayNodeProvider）

## 来源与目标

| 源文件（C#） | 目标文件（Rust） |
| --- | --- |
| `Qomicex.Connector/RelayNodes.cs` | `src/relay/nodes.rs` |
| `Qomicex.Connector/RelayNodeProvider.cs` | `src/relay/provider.rs` |
| `Qomicex.Connector.Tests/RelayNodeProviderTests.cs` | `provider.rs` 内 `#[cfg(test)] mod tests` |
| — | `src/relay/mod.rs`（新增模块根，lib.rs 挂载 `pub mod relay;`） |

## 新增依赖

- `sys-locale = "0.3"`（加入 `crates/qomicex-connector/Cargo.toml`）— 用于系统地区检测，替代 C# 的 `RegionInfo.CurrentRegion.TwoLetterISORegionName`。

## 映射说明（一一对应）

| C# | Rust | 说明 |
| --- | --- | --- |
| `RelayNodes.Default` | `pub const DEFAULT_NODES: [&str; 2]` | 类型由 `IReadOnlyList<string>` 改为 `[&str; 2]` 常量 |
| `RelayNodes.Resolve` | `pub fn resolve(Option<&[String]>, Option<&[String]>) -> Vec<String>` | 逻辑一致：override 有则用之，否则默认，再追加 additional |
| `RelayNodeProvider.Endpoint` | `pub const ENDPOINT: &str` | 一致 |
| `RelayNodeProvider.DefaultUserAgent` | `pub const DEFAULT_USER_AGENT: &str` | 一致 |
| `RelayNodeProvider(userAgent, preferredRegion, logger, handler)` | `new(user_agent: Option<String>, preferred_region: Option<String>)` | logger / handler 参数按约束移除 |
| `DetectSystemRegion()` | `fn detect_system_region()` | `sys_locale::get_locale()` → 取 `-` 后段大写；无 `-` 或失败回退 `"CN"` |
| `FetchAsync(ct)` | `fetch()` → `fetch_from(ENDPOINT)` | 按约束拆分出可注入的 `fetch_from(&self, url: &str)` |
| `ResolveHttpNodesAsync` / `ResolveSingleNodeAsync` | `fetch_nodes` / `resolve_http_node` | http(s) 节点二次 GET，返回体 trim 后作为实际节点，失败跳过；跳过节点则记日志 |
| `ParseNodes(json, preferredRegion)` | `pub fn parse_nodes(&str, Option<&str>) -> Vec<String>` | serde_json 解析；region 大小写不敏感匹配排前，其余保序在后；JSON 非法/非数组返回空 |
| HTTP 超时 | `reqwest::Client::builder().timeout(Duration::from_secs(10))` | 一致 |
| 日志 | `log::info!` / `log::warn!` | 文案与 C# 一致（中文） |

## 测试方案

- ParseNodes 8 个测试全部移植（ValidObjectArray / MissingOrEmptyUrl / InvalidJson / NotArray / PreferredRegion_MatchingFirst / CaseInsensitive / NoPreferredRegion / NoMatchingRegion），直接调用 `parse_nodes` 断言。
- FetchAsync 测试：本地 `tokio::net::TcpListener` 绑定 `127.0.0.1:0` 随机端口，`start_server` helper 手动构造 HTTP/1.1 响应（含 Content-Length），逐字节累积读到 `\r\n\r\n` 解析请求头并记录；测试通过 `provider.fetch_from("http://{addr}/nodes")` 注入本地地址。
- 移植的 Fetch 测试：ValidResponse / HttpError(500) / EmptyArray / SendsCustomUserAgent / UsesDefaultUserAgent / RequestsConfiguredEndpoint。
- 测试中 client builder 设置 `.no_proxy()`（生产与测试共用同一 builder，均生效），避免本地连接走系统代理。

## 偏差 / 决策记录

1. **`.no_proxy()` 全局生效**：Rust 版无法像 C# 那样注入 HttpMessageHandler，为满足"测试中避免本地连接走代理"，共享 builder 直接 `.no_proxy()`，生产环境也禁用了系统代理检测。
2. **`FetchAsync_PreferredRegion_SortsMatchingFirst` 未移植（UNMAPPED）**：该 C# 测试与实现自相矛盾——实现会对所有 https:// 节点发起二次解析请求，而 StubHandler 对任何请求都返回同一 JSON 数组，导致二次解析结果为 JSON 文本而非期望的节点顺序，断言无法成立。对应排序行为已由 ParseNodes 的 4 个相关测试覆盖。
3. **`FetchAsync_RequestsConfiguredEndpoint` 适配**：无法在测试中直连真实端点，改为断言本地请求行 `GET /nodes HTTP/1.1`（证明 fetch_from 使用的 URL 被正确请求）并断言 `ENDPOINT` 常量值。
4. **取消令牌（CancellationToken）**：按约束未移植，Rust 版无取消支持。
5. **日志无异常详情**：Rust 侧 `log` crate 的 warn 文案与 C# 一致，但不携带底层异常信息（`?` 直接传播失败）。

## 未覆盖（UNMAPPED 汇总）

- `FetchAsync_PreferredRegion_SortsMatchingFirst`（见偏差 2）
- CancellationToken 取消语义（见偏差 4）
- `ILogger<T>` 结构化日志参数（Rust 用 log crate 文本日志替代）
