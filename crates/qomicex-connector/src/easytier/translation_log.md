# 翻译日志：EasyTierManager.cs → src/easytier/manager.rs

- 源：`Qomicex.Connector/EasyTierManager.cs`（进程版，spawn `easytier-core`）
- 目标：`crates/qomicex-connector/src/easytier/{mod.rs, manager.rs}`（easytier 库版，进程内运行）
- 约束：不修改 MAPPING_TABLE.yaml；未执行 cargo/git/测试

## 公共 API（固定签名）

| C# | Rust |
|----|------|
| `EasyTierManager(ILogger)` | `EasyTierManager::new() -> Self`（内部 `Arc::new(native_instance_manager())`；日志改用 `log` crate） |
| `IsRunning` | `is_running(&self) -> bool`（instance_id 存在且 `instance.is_ready()`） |
| `VirtualIp` | `virtual_ip(&self) -> Option<String>`（缓存） |
| `NodeId` | `node_id(&self) -> Option<String>`（缓存） |
| `InstanceId` | 内部字段 `instance_id: Option<Uuid>`（C# 从日志正则解析；Rust 直接 `run_network_instance` 返回值，无公开 getter） |
| `StartAsync(config, ct)` | `start(&mut self, config: &NetworkConfig) -> Result<(), ScaffoldingError>`（30s 超时 → `EasyTierTimeout("EasyTier 启动超时 (30s)")`；成功 sleep 2s 对齐 2000ms 缓冲） |
| `StopAsync(ct)` | `stop(&mut self) -> Result<(), ScaffoldingError>`（`delete_network_instances`，清空缓存；异常仅 warn 不传播，对齐 C# catch） |
| `GetNodesAsync(ct)` | `get_nodes(&self) -> Vec<EasyTierNode>`（`route_snapshots()` 映射；无需 CLI） |
| —（C# 无对应；新增能力） | `apply_config_patch(&self, patch: InstanceConfigPatch) -> Result<(), ScaffoldingError>` | 进程内直接调 `easytier_core::management::apply_config_patch`，运行时覆盖配置，**不重启实例、不断开网络连接**；实例必须处于运行状态（见下方"动态配置修改"） |
| `Dispose()` | Drop 时由 InstanceManager 自动回收（未实现 Drop，无进程需杀） |

## TomlConfig 构建决策（BuildArgs 映射）

1. **ListenRandomPorts**：选 **两个 port-0 URL**（`tcp://0.0.0.0:0` + `udp://0.0.0.0:0` → `set_listeners`），与 C# `-l` 语义完全一致。
   弃用 `set_listeners(vec![])`：源码确认空列表 = **完全不监听（仅出站连接）**，与 C# "随机端口监听" 语义不同（easytier factory 自带测试即用空列表跑无 listener 实例）。
2. **ipv4 掩码**：C# 传纯 IP（`--ipv4 10.144.144.1`）；`set_ipv4` 需要 `cidr::Ipv4Inet`，纯 IP 时拼接 `/24`。旁证：core 的 `get_ipv4` 内部对 /32 也归一化为 /24，两种写法结果一致。
3. **默认中继节点**：`config.relay_nodes == None` → `relay::nodes::resolve(None, None)`（内置 `DEFAULT_NODES`），对应 C# `RelayNodes ?? RelayNodes.Default`。HTTPS 节点列表 URL 由 core 手动连接器原生支持（`connectivity/manual/mod.rs` scheme 白名单含 http/https）。
4. **超时不清实例**：对齐 C#（超时抛异常但进程存活），返回 `EasyTierTimeout` 时实例保持运行。
5. **flags**：`--compression=zstd --multi-thread --latency-first --enable-kcp-proxy` → `data_compress_algo = CompressionAlgoPb::Zstd.into()`（prost 枚举转 i32）、`multi_thread`、`latency_first`、`enable_kcp_proxy` 置 true；`--no-tun`/`--use-smoltcp` 来自 config。
6. **白名单**：`set_tcp_whitelist(["0"] + config.tcp_whitelist)`、`set_udp_whitelist(["0"])`。
7. **端口转发**：`tcp://127.0.0.1:LOCAL/REMOTE:PORT` → `PortForwardConfig { bind_addr, dst_addr, proto }`（core 侧 `easytier-core/src/config/gateway.rs` 字段确认）。

## 类型路径确认（源码核对，非猜测）

- `easytier::instance::factory::{native_instance_manager, NativeInstanceFactory}` — `management` 特性含 `management-rpc`，可用。
- `easytier_core::instance::manager::{InstanceManager, ConfigFileControl::STATIC_CONFIG}`。
- `easytier_core::config::toml::{ConfigLoader as _, NetworkIdentity, TomlConfig, PeerConfig, PortForwardConfig}`（toml.rs 重导出 `gateway::PortForwardConfig`）。
- `easytier_proto::core_peer::peer::Route` — ⚠️ spike 写 `easytier::proto::core_peer::peer::Route` 不存在：`easytier::proto` 只重导出 `acl/common/core_config/error/peer_rpc`，**不含 core_peer**。直接用 `easytier_proto`。
- `Route { peer_id: u32, ipv4_addr: Option<Ipv4Inet>, hostname: String }`（generated OUT_DIR 确认）。
- ⚠️ **两种 Ipv4Inet 易混淆**：
  - `Route.ipv4_addr` = prost `easytier_proto::common::Ipv4Inet`，字段为 `address: Option<Ipv4Addr>` + **`network_length: u32`**（spike 写的 `prefix_len` 是本 fork 的字段名，错误）。取 IP：`std::net::Ipv4Addr::from(*addr)`（common.rs:76 有 From）。
  - `NodeSnapshot.ipv4_addr` = `cidr::Ipv4Inet`（std cidr crate），取 IP：`.address().to_string()`。
- `FlagsInConfig.data_compress_algo: i32`（prost 枚举字段为 i32，`CompressionAlgoPb::Zstd.into()` 可用）。
- `instance.is_ready()/peer_id()/node_snapshot()/route_snapshots()` 为 `CoreInstance` 固有方法（private `mod management` 中的固有 impl 对外可用）。
- `peer_id() -> PeerId = u32`（config/mod.rs:48 `pub type PeerId = u32`）。

## 新增依赖

- `uuid = "1"`（workspace）：`instance_id: Option<Uuid>` 必须命名类型，无重导出。
- `cidr = { version = "0.3.1", features = ["serde"] }`（workspace）：`set_ipv4` 参数类型必须命名。
- 两者均已存在于 Cargo.lock（uuid 1.24.0 / cidr 0.3.2），无需手动改 lock；但 qomicex-connector 的依赖列表需 cargo 重新解析（未执行 cargo）。

## UNMAPPED / 语义差异

- `EasyTierPath`（C# 可指定 exe 路径）→ 无对应项（库模式无进程，**UNMAPPED**，记录待上层删除/忽略）。
- `FindEasyTier/FindExecutable` → 删除（库模式不需要）。
- 日志正则解析 virtual_ip/node_id/instance_id → 替换为 `node_snapshot()/peer_id()` 直接读取（更可靠）。
- `TryGetNodesViaCli`（spawn easytier-cli peer）→ 替换为 `route_snapshots()`。
- `_outputLines` 缓冲 → 删除（无输出流）。
- DHCP 节点（Guest）的虚拟 IP 取 `node_snapshot().ipv4_addr`；Host 固定 IP 直接用 config.ipv4（去 /24），与 C# 解析日志结果一致。
- C# `VirtualIp`/`NodeId` 在启动前重置为 null；Rust `start()` 开头同步重置缓存。
- 超时轮询：C# 用 TaskCompletionSource 事件驱动；Rust 改为 500ms 轮询 `is_ready()`（≤30s），等价且为 spike 指定方案。

## 动态配置修改（apply_config_patch）

- **底层机制**：`easytier_core::management::apply_config_patch(instance: &Arc<CoreInstance<H>>, patch: InstanceConfigPatch)` 为公开 API（`management` feature 下导出）。语义：以启动时的共享 `TomlConfig` 为基座，将 `InstanceConfigPatch` 作为运行时覆盖打上去（先 `detached_snapshot()` 拷贝候选 → 逐项 patch → 校验 → `replace_from_snapshot` 提交回共享 TomlConfig → 同步运行时配置），实例**不重启**。
- **前置条件**：实例状态必须为 `Running`（`start()` 轮询就绪之后才能调），否则报错 "instance is not ready"。
- **可 patch 字段**：hostname、ipv4/ipv6、port_forwards、acl、proxy_networks、routes、exit_nodes、mapped_listeners、connectors、ipv6_public_addr_*、disable_relay_data。
- **集合字段为增量语义**：`port_forwards` 等按 `ConfigPatchAction::Add/Remove/Clear` 增量生效（不是全量替换）。底层 `PortForwardAdapter::reload` 对每条规则做 diff：保留仍在的、新增缺失的、取消多余的任务（`easytier-core/src/gateway/port_forward.rs`）。
- **feature 前提**：port_forward patch 要求编译 `proxy-smoltcp-stack`（workspace `easytier-core` 已启用），否则校验直接拒绝。
- **本仓库用途**：guest 端在 `connect()` / `map_minecraft_port()` 中动态 ADD 端口转发规则，替代原先 stop/start 重启 EasyTier 的方案（见 `src/guest/translation_log.md` 决策 9）。
