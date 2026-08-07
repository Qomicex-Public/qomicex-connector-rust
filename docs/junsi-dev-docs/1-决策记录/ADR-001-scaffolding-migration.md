# ADR-001: Scaffolding-MC 协议 C# → Rust 移植架构决策

日期：2026-08-07
状态：已接受
范围：Qomicex.Connector.Part.Scaffolding（C#）→ qomicex-connector-rust

## 背景

将 C# 实现的 Scaffolding-MC 联机协议库移植为 Rust。C# 版通过 spawn `easytier-core`
进程 + 解析 stdout 的方式使用 EasyTier；Rust 版要求改为 **EasyTier 库内嵌**
（`easytier`/`easytier-core` crate），并保持 SCF 协议 wire 格式逐字节兼容
（互操作 golden 测试已覆盖）。

## 决策

### D1：EasyTier 以 git 依赖接入（非 path/workspace）
- `easytier` / `easytier-core` / `easytier-proto` 均为 git 依赖
  `ssh://git@github.com/Qomicex-Public/EasyTier4QML.git`
- features 显式声明精简集：`aes-gcm, endpoint-discovery, extended-services,
  management, tcp-hole-punch, zstd, smoltcp`
- 理由：版本独立、构建隔离；SSH 而非 HTTPS（libgit2 直连 github 失败）

### D2：Host 也用 smoltcp 无 TUN（与 Guest 一致）
- C# Host 默认 TUN 模式；Rust 版 Host 改为 `no_tun + use_smoltcp`，
  TCP 数据面由 easytier 用户态协议栈处理（`wrapped_tcp_proxy` 将
  发往虚拟 IP 的 TCP 重写到 127.0.0.1）
- 理由：库内嵌场景无管理员权限、无需 wintun 驱动，集成最干净
- 影响：Host 本机无法直接访问虚拟 IP（`10.144.144.1` 仅在 easytier 栈内可路由），
  与 C# 的 TUN 行为不同；对访客侧无感知（访客本来就走 no-tun）

### D3：workspace 结构 = lib + bin
- `crates/qomicex-connector`（库，对应 C# 三项目中的类库）
- `crates/qomicex-connector-cli`（bin，对应 Console 示例，新增 relay 子命令）

### D4：启动完成检测 = `instance.is_ready()` 轮询
- 替代 C# 解析 stdout "listener added"；超时 30s + 成功后再等 2s 缓冲

### D5：节点列表 = `instance.route_snapshots()`
- 替代 C# spawn `easytier-cli peer` 解析表格；Route 含 peer_id/hostname/ipv4_addr

### D6：NodeId = `instance.peer_id()`；VirtualIp = node_snapshot / 固定 ipv4
- Host 固定 `10.144.144.1`；Guest DHCP 从 node_snapshot 提取

### D7：日志用 `log` crate（CLI 内 env_logger），事件用 watch/mpsc
- 替代 C# `ILoggerFactory` 注入与 `event`；`PlayersChanged` → watch channel，
  `ClientDisconnected` → mpsc，`ConnectionLost` → watch

### D8：PlayerPing 容错为有意改进（非逐字节对齐 C#）
- C# 对畸形 JSON 抛异常断连且不写响应；Rust 缺失属性回退空串、
  解析失败回 status 255 保持连接（255 不刷新心跳 → 畸形客户端 15s 后被剔除）
- 理由：255 是 SCF 通用错误通道，断连破坏错误模型一致性

## 备选方案

| 方案 | 理由 | 否决原因 |
|---|---|---|
| path 依赖 + workspace 合并 EasyTier4QML | 便于调试 | 仓库强耦合、构建相互影响 |
| Host 用 TUN | 与 C# 逐字节一致 | 需驱动安装/管理员权限 |
| 单 crate 内置 bin | 简单 | 库/bin 关注点耦合 |

## 影响

- 迁移分支 `migrate/qomicex-connector`，分批 checkpoint（Batch1-5）
- 35 个单元测试（含 C# 帧格式 golden 互操作测试）
- QA 报告：wire 协议级零差异；2 个 medium 已修复（custom_protocols 透传、
  PlayerPing 注释），8 个 low 记为可接受偏差（见 docs/qa_report.md）

## 技术债

1. **中继在线获取**依赖 `nodes.qomicex.top`：不可达时正确回退内置默认节点
2. **easytier features 精简集**：未启用 kcp/quic/wireguard/faketcp/magic-dns；
   若中继节点仅支持这些协议（如 wss/kcp 端口）可能连接失败——可通过
   追加 features 解决
3. **P2P 打洞/中继数据面**：需真实跨机器网络环境验证（localhost 打洞必然失败），
   本机已验证控制面（发现/协商）与直连拓扑数据面
4. **`create_room` 参数顺序**：`custom_protocols` 置于 `ct` 之后（对齐 C# 参数序），
   与 `join_room` 的 `custom_protocol_keys` 位置一致，但 API 一致性可再评审
5. translation_log*.md 散落于 src/ 下，可考虑归入 docs/
