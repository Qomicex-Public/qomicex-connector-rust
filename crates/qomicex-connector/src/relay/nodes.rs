//! 内置默认中继节点与节点列表解析工具。

/// 内置默认中继节点。
///
/// ⚠️ 必须为 easytier 可直接连接的协议 URI（tcp/udp/quic/...）——旧实现直接抄 C#
/// 的 `https://etnode.zkitefly.eu.org/nodeN` 形式，而 easytier 不支持 https peer
/// scheme，fallback 触发时中继列表全部无效。此处用其解析后的实际地址。
pub const DEFAULT_NODES: [&str; 2] = [
    "tcp://cgk1.clusters.zeabur.com:22171",
    "tcp://tcp.ap-northeast-1.clawcloudrun.com:45146",
];

/// 解析最终节点列表：override 非空则使用之，否则使用默认节点，最后追加 additional。
pub fn resolve(override_nodes: Option<&[String]>, additional: Option<&[String]>) -> Vec<String> {
    let mut result: Vec<String> = match override_nodes {
        Some(nodes) => nodes.to_vec(),
        None => DEFAULT_NODES.iter().map(|s| s.to_string()).collect(),
    };
    if let Some(additional) = additional {
        result.extend(additional.iter().cloned());
    }
    result
}
