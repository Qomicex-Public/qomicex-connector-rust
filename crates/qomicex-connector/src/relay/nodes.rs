//! 内置默认中继节点与节点列表解析工具。

/// 内置默认中继节点。
pub const DEFAULT_NODES: [&str; 2] = [
    "https://etnode.zkitefly.eu.org/node1",
    "https://etnode.zkitefly.eu.org/node2",
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
