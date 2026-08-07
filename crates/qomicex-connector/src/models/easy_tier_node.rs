//! EasyTier 网络节点。

/// EasyTier 网络节点。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EasyTierNode {
    /// 虚拟 IP。
    pub virtual_ip: String,
    /// 主机名。
    pub hostname: String,
    /// 节点 ID。
    pub node_id: String,
}
