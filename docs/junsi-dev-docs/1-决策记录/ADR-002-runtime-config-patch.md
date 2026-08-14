# ADR-002: Guest 端口转发改用运行时配置 Patch（apply_config_patch）

日期：2026-08-11
状态：已接受
范围：qomicex-connector（guest 端 EasyTier 端口转发路径）

## 背景

Guest 加入房间后，若直连房主虚拟 IP 失败，需要建立 `127.0.0.1` 端口转发
（Center 转发）；`map_minecraft_port` 在转发模式下还需追加一条 MC 转发。
原实现（对齐 C#）通过 **stop/start 重启 EasyTier 实例**来应用新的
`port_forwards` 配置，代价：

- EasyTier 实例整体重建：P2P 打洞/连接需要重新建立；
- Center TCP 连接断开后重连，心跳停启，期间可能触发连接丢失通知；
- `map_minecraft_port` 需要先断开 Center 再重连，路径长、抖动大。

底层库（`easytier-core`，rev 287c667）已提供动态修改运行中实例配置的公开
API `easytier_core::management::apply_config_patch`，无需重启即可增量生效。

## 决策

### D1：新增 `EasyTierManager::apply_config_patch`
- `crates/qomicex-connector/src/easytier/manager.rs` 新增公开方法，
  进程内直接调用 `easytier_core::management::apply_config_patch`；
- 实例未启动 / 不存在时返回 `ScaffoldingError::EasyTierStart`。

### D2：Guest 两处重启路径改为动态 ADD patch
- `connect()` 直连失败 fallback：由 `stop → 改 config → start` 改为
  `apply_config_patch` ADD Center 转发，EasyTier 实例与网络连接保持；
- `map_minecraft_port()` 转发模式：只 ADD MC 转发，**不再**停心跳、
  断 TCP、重启、重连 Center；
- `self.config.port_forwards` 存储仍同步更新（增量 push），保持配置一致。

### D3：辅助函数
- 新增私有 `apply_port_forward_patch(forward)`：解析
  `tcp://127.0.0.1:LOCAL/REMOTE:PORT` → `PortForwardConfigPb`，构造
  `InstanceConfigPatch { port_forwards: [Add] }`；
- `parse_local_port_from_forward` 替换为 `parse_forward_addrs`（返回
  `(bind_addr, dst_addr)`）。

## 理由

- `apply_config_patch` 是进程内公开 API，参数类型
  `&Arc<CoreInstance<H>>` 与 `manager.instance(id)` 返回值直接匹配，
  无需引入 RPC；
- 集合字段（port_forwards）为 ADD/REMOVE/CLEAR **增量**语义，底层
  `PortForwardAdapter::reload` 只对差异做增删，绑定新 listener 即可，
  实例状态、P2P 连接、监听中的旧规则全部保持；
- feature 前提已满足：workspace `easytier-core` 已启用 `management` +
  `proxy-smoltcp-stack`（`build_capabilities` 校验 port_forward 非空时
  要求 smoltcp gateway）。

## 备选方案

| 方案 | 理由 | 否决原因 |
|---|---|---|
| 保持 stop/start 重启 | 与 C# 行为一致 | P2P/Center/心跳全断，抖动大；`map_minecraft_port` 需重连 Center |
| 通过 CLI/RPC patch_config | 与 CLI 同路径 | 进程内已有公开 API，走 RPC 徒增链路 |
| 修改底层库 | 换全量 replace 语义 | 增量 ADD 已满足需求，无需动依赖 |

## 影响

- 行为改进：转发建立期间 Center TCP 连接与心跳**不断**，连接丢失通知不会
  被误触发；EasyTier P2P 连接不重建；
- 失败语义：`apply_config_patch` 失败（如本地端口被占）时错误向上传播，
  调用方需自行决定是否回退到重启路径（当前未实现回退）；
- `map_minecraft_port` 的 `ct` 参数保留但改名为 `_ct`（签名兼容 CLI 调用处）；
- 编译验证：`cargo check -p qomicex-connector` 通过，35 个单元测试全部通过。

## 技术债

1. **无回退路径**：patch 失败时无 stop/start 兜底，极端情况（端口竞态被占）
   可能无法建立转发；后续可加回退。
2. **`config.port_forwards` 与运行实例的一致性**：当前靠手工同步 push；
   若未来多次 ADD/REMOVE 交错，建议抽象为统一的 forward 集合管理。
