# 技术债清单：qomicex-connector-rust（Scaffolding C# → Rust 移植）

日期：2026-08-07
依据：docs/qa_report.md、docs/diagnose_report.md、docs/ADR-001、各 translation_log

优先级：P0=阻塞上线 / P1=应尽快 / P2=可延后 / P3=优化项

---

## 一、行为偏差（与 C# 有意不同，已记录）

### 1.1 PlayerPing 容错语义 — P2（已修复文档，行为保持）
- **现状**：C# 对畸形/缺字段 player_ping 抛异常→整条连接断开且不写响应；Rust 缺失键回退空串、JSON 非法回 255 + 连接保持。
- **判定**：有界容错（255 不刷新心跳 → 畸形客户端 ≤15s 被心跳剔除），统一了 SCF 255 错误通道，优于 C# 的异常断连。
- **遗留**：仅注释已修正（protocols/mod.rs:162）。若未来发现 255 容错被恶意利用（如刷连接），可收紧为「player_ping 255 → 断连」。

### 1.2 discover() 无取消传播 — ✅ 已修复（2026-08-07）
- C# `DiscoverAsync(ct)` 取消立即中断 60×500ms 扫描；Rust 原 `discover(get_nodes)` 无 ct 参数，取消后仍跑满 30s。
- **修复**：`discover(get_nodes, ct)` 加 `&CancellationToken`，sleep 处 `tokio::select!` 取消即返回；调用点（scaffolding_guest.rs connect）已传 ct。

### 1.3 HeartbeatService 定时漂移 — P3
- C# PeriodicTimer 固定 5s 节奏；Rust `sleep(interval)` 回调结束后重置（回调耗时 >0 时漂移）。
- **影响**：心跳实际间隔略大于 5s，远小于 15s 超时窗口，无实际影响。

### 1.4 心跳超时 ClientDisconnected 触发次数 — P3
- C# 超时路径触发 2 次（显式 invoke + finally）；Rust 只 1 次。
- **影响**：玩家移除结果一致，仅 C# 多一次空 machineId 通知（多一次 PlayersChanged 事件）。Rust 行为更干净，保持。

### 1.5 is_connected 状态过期 — P3
- C# `client.Connected` 轮询 socket 实时状态；Rust `stream.is_some()`，对端断线后返回 true 直到下次 send 失败。
- **影响**：仅影响心跳任务的「未连接提前返回」判断，send 失败会真实报错，风险低。可改为 peek/写探测。

### 1.6 region 检测格式差异 — P3
- C# `RegionInfo.TwoLetterISORegionName`；Rust `sys_locale` 取 `-` 后段（`en_US` 下划线格式回退 "CN"）。
- **影响**：仅影响中继节点排序优先级，hyphen 格式系统（zh-CN 等主流）等价。

### 1.7 machine_id[..8] 字节切片 — ✅ 已修复（2026-08-07）
- C# 字符切片安全；Rust 原 `&machine_id[..len.min(8)]` 字节切片，machine_id 含非 ASCII（中文/emoji）时 **panic**。
- **修复**：改为 `machine_id.chars().take(8).collect::<String>()`（字符切片，非 ASCII 安全）。

### 1.8 ASCII 解码 U+FFFD vs '?' — P3
- 反序列化畸形 type 串时 Rust `from_utf8_lossy` → U+FFFD，C# `ASCII.GetString` → '?'。
- **影响**：仅畸形 type（非 ASCII 字节）触发，且随后命中 `split_type_str` 报错，结果不可区分。

---

## 二、功能缺口

### 2.1 Host 自定义协议重复键检查 — ✅ 已修复（2026-08-07）
- C# `ToDictionary(p => p.ProtocolKey)` 遇重复键（自定义撞标准键/互撞）抛错 → 开房失败；Rust HashMap collect 静默覆盖。
- **修复**：start() 装配前校验标准键与自定义键互斥 + 自定义键去重，冲突返回 `Protocol("自定义协议键冲突: {k}")`（scaffolding_center.rs:121-128）。

### 2.2 ProtocolHandler trait 未显式 'static — P3
- `pub trait ProtocolHandler: Send + Sync` 无显式 `+ 'static`，trait object 默认隐式携带。显式化可改善报错信息，行为零变化。

### 2.3 assemble_protocols 纯函数缺失 — P3
- diagnose 建议把「标准键+自定义键 → (advertised_keys, protocols)」抽成纯函数以便单测；当前在 start() 内联，无直接单测。

---

## 三、环境/网络类

### 3.1 中继在线获取依赖外部服务 — P2
- `https://nodes.qomicex.top/api/nodes` 在部分网络不可达；失败正确回退内置默认节点（etnode.zkitefly.eu.org node1/node2），但本机测试时默认节点也解析失败 → 无中继可用。
- **验证**：需在可访问该服务或自有中继的真实网络下验证「在线获取 → 排序 → 回退」全链路。

### 3.2 easytier features 精简集 — ✅ 已扩展（2026-08-07）
- 已启用：`aes-gcm, endpoint-discovery, extended-services, management, tcp-hole-punch, zstd, smoltcp, kcp, quic, faketcp`（easytier-core 同步 `proxy-packet`；easytier-proto 同步 `quic, faketcp`）。
- udp 为 easytier 基础协议，无需 feature。
- **注意**：启用 faketcp 后 Windows 链接需要 `Packet.lib`（easytier build.rs 输出相对 LIBPATH `easytier/third_party/x86_64/`，按 rustc CWD=workspace 根解析）→ 已把 Packet.lib/Packet.dll 放入工作区根 `easytier/third_party/x86_64/`（勿删）。
- 若仍需 wireguard/websocket/magic-dns：追加对应 feature（编译成本递增）。

### 3.3 P2P 打洞/中继数据面未真实验证 — P1（部分验证）
- 本机已验证：控制面（开房/加入/发现/协商/路由同步）全通、**端口转发数据面全链路**（集成测试 `tests/e2e_direct.rs`，`#[ignore]` 手动触发，验证：发现中心 → 端口转发连接 → 协商 → player_ping → ping → server_port → 玩家列表 → 255）。
- **关键修复**：easytier `gateway_enabled` 依赖 `socks5` feature，未启用时端口转发监听器不启动（10061）→ 已加 `socks5` feature。
- **剩余**：真实跨机器 + 公网中继下 P2P 打洞（localhost 打洞必然失败，需用户实测）。

### 3.4 EasyTier 同机多实例限制 — P3
- 同机三进程（relay+host+guest）时打洞连接握手失败（`conn closed during wait handshake response`），属 easytier 组网行为，非移植代码缺陷。跨机器环境预期正常。

---

## 四、代码质量/工程

### 4.1 translation_log 散落 src/ 下 — P3
- `src/**/translation_log*.md` 共 8 个文件散落在源码目录，可归入 docs/ 或删除（信息已由 ADR/QA/本清单覆盖）。

### 4.2 create_room 参数顺序可读性 — P3
- `create_room(name, machine_id, vendor, port, ct, custom_protocols)` 中 ct 在 custom_protocols 之前（对齐 C# 参数序），与 `join_room` 的 `custom_protocol_keys` 位置一致但视觉不对称。可后续评审统一。

### 4.3 easytier 依赖版本锁定 — ✅ 已修复（2026-08-07）
- git 依赖已锁定 `rev = "287c667"`（EasyTier4QML 当前 HEAD），上游变更不再破坏构建；升级需显式改 rev。

### 4.4 release 构建体积 — P3
- release exe 约 27MB（dev 带全部 easytier 模块）。可评估 `strip` / LTO（workspace profile 已有 lto=true + strip=true）/ feature 裁剪。

---

## 五、验证缺口

### 5.1 集成测试无法自动化 — ✅ 已建脚手架（2026-08-07）
- `crates/qomicex-connector/tests/e2e_direct.rs`（`#[ignore]` 标记，手动触发）：
  `cargo test -p qomicex-connector --test e2e_direct -- --ignored --nocapture`
- 直连拓扑（host 固定 listener + guest 端口转发）全流程通过；跨机器/中继打洞场景需真实网络，仍无法 CI 自动化。

### 5.2 C# ↔ Rust 跨语言互操作实测 — P2
- 已做字节级 golden test（帧格式），但未与真实 C# 端（Qomicex.Connector）联机互操作实测。
- **建议**：跨机器验证时一端跑 C# Console、一端跑 Rust CLI，确认协议互通。

---

## 修复建议顺序

1. **已修复**：2.1 重复键检查、1.7 字节切片、1.2 discover 取消、4.3 依赖锁 rev、3.2 features 扩展（含 socks5，修复端口转发）、3.3 本机数据面全链路、5.1 集成测试脚手架（2026-08-07）
2. **P1 剩余**：3.3 跨机器 P2P 打洞实测（用户实测）
3. **P2**：3.1 中继服务验证、5.2 跨语言实测
4. **P3**：1.1/1.3/1.4/1.5/1.6/1.8 行为偏差（保持现状，文档已记录）、2.2/2.3/4.1/4.2/4.4 工程优化
