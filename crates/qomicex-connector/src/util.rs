//! 取消令牌：轻量的线程安全取消信号（对应 C# `CancellationToken`）。

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::Notify;

/// 可克隆的取消令牌：`cancel()` 触发，所有等待方在 `cancelled()` 处唤醒。
#[derive(Clone, Default)]
pub struct CancellationToken {
    state: Arc<TokenState>,
}

#[derive(Default)]
struct TokenState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    /// 创建未取消的令牌。
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否已取消。
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    /// 触发取消（幂等）。
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
        self.state.notify.notify_waiters();
    }

    /// 等待取消；若已取消则立即返回。
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.state.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// 已连接物理网卡的候选 IPv4 列表（排除虚拟 VPN 网卡、回环与 APIPA）。
///
/// 排序：有线（Ethernet/以太网）优先 → 无线（WLAN/Wi-Fi/无线）→ 其他；
/// 未连接但启用的网卡（如没插线的 LAN）没有有效 IPv4（仅 APIPA/无地址），
/// 自然被排除，剩下已连接的 WLAN 等。
pub fn physical_ipv4_candidates() -> Vec<String> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};
    let Ok(interfaces) = NetworkInterface::show() else {
        return Vec::new();
    };
    let mut candidates: Vec<(String, String)> = Vec::new();
    for iface in interfaces {
        let lower = iface.name.to_ascii_lowercase();
        if is_virtual_interface(&lower) {
            continue;
        }
        for addr in &iface.addr {
            let network_interface::Addr::V4(v4) = addr else {
                continue;
            };
            if v4.ip.is_loopback() || v4.ip.is_link_local() {
                continue;
            }
            candidates.push((lower.clone(), v4.ip.to_string()));
        }
    }
    sort_candidates(&mut candidates);
    candidates.into_iter().map(|(_, ip)| ip).collect()
}

/// 排序：有线（Ethernet/以太网）优先 → 无线（WLAN/Wi-Fi/无线）→ 其他，同档按名称稳定。
fn sort_candidates(candidates: &mut [(String, String)]) {
    let score = |n: &str| -> u8 {
        if n.contains("ethernet") || n.contains("以太网") {
            0
        } else if n.contains("wlan") || n.contains("wi-fi") || n.contains("wifi") || n.contains("无线") {
            1
        } else {
            2
        }
    };
    candidates.sort_by_key(|(n, _)| (score(n), n.clone()));
}

/// 解析 easytier 出站绑定 IP：取第一个候选物理网卡地址（无候选返回 None，
/// 调用方回退 0.0.0.0 由系统选择）。
pub fn resolve_bind_ip() -> Option<String> {
    physical_ipv4_candidates().into_iter().next()
}

/// 虚拟/VPN 网卡名关键词（命中即视为虚拟网卡，不作为绑定候选）。
fn is_virtual_interface(lower: &str) -> bool {
    const VIRTUAL_KEYWORDS: &[&str] = &[
        "radmin",
        "hamachi",
        "zerotier",
        "wintun",
        "tailscale",
        "wireguard",
        "openvpn",
        "nordvpn",
        "surfshark",
        "vmware",
        "virtualbox",
        "virtual",
        "loopback",
        "easytier",
        "vpn",
        "tun",
        "tap",
    ];
    VIRTUAL_KEYWORDS.iter().any(|k| lower.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_interface_detection() {
        assert!(is_virtual_interface("radmin vpn"));
        assert!(is_virtual_interface("wintun"));
        assert!(is_virtual_interface("hamachi"));
        assert!(is_virtual_interface("zerotier one"));
        assert!(is_virtual_interface("openvpn tap"));
        assert!(!is_virtual_interface("ethernet"));
        assert!(!is_virtual_interface("wlan"));
        assert!(!is_virtual_interface("以太网"));
        assert!(!is_virtual_interface("wi-fi"));
    }

    #[test]
    fn bind_ip_candidates_prefer_wired() {
        let mut cands = vec![
            ("wlan".to_string(), "192.168.1.5".to_string()),
            ("ethernet".to_string(), "192.168.1.2".to_string()),
            ("radmin vpn".to_string(), "10.0.0.7".to_string()),
        ];
        sort_candidates(&mut cands);
        assert_eq!(cands[0].0, "ethernet");
        assert_eq!(cands[1].0, "wlan");
        assert_eq!(cands[2].0, "radmin vpn");
    }
}
