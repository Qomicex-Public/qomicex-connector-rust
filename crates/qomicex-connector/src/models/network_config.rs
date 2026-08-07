//! EasyTier 网络配置。

/// EasyTier 网络配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkConfig {
    /// 网络名称。
    pub network_name: String,
    /// 网络密钥。
    pub network_secret: String,
    /// 本机主机名。
    pub hostname: String,
    /// 是否启用无 TUN 模式（默认 true）。
    pub no_tun: bool,
    /// 是否使用 smoltcp（默认 false）。
    pub use_smoltcp: bool,
    /// 是否启用 DHCP（默认 true）。
    pub dhcp: bool,
    /// 固定 IPv4 地址（默认 None）。
    pub ipv4: Option<String>,
    /// 是否使用随机监听端口（默认 true）。
    pub listen_random_ports: bool,
    /// TCP 白名单（默认空）。
    pub tcp_whitelist: Vec<String>,
    /// 端口转发规则（默认空）。
    pub port_forwards: Vec<String>,
    /// 中继节点列表（默认 None）。
    pub relay_nodes: Option<Vec<String>>,
}

impl Default for NetworkConfig {
    /// 与 C# 默认值保持一致（bool 字段默认并非全 false）。
    fn default() -> Self {
        Self {
            network_name: String::new(),
            network_secret: String::new(),
            hostname: String::new(),
            no_tun: true,
            use_smoltcp: false,
            dhcp: true,
            ipv4: None,
            listen_random_ports: true,
            tcp_whitelist: Vec::new(),
            port_forwards: Vec::new(),
            relay_nodes: None,
        }
    }
}
