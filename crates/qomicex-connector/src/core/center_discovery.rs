//! 联机中心发现：扫描 EasyTier 节点，按主机名约定匹配中心及其端口。

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;

use crate::error::ScaffoldingError;
use crate::models::easy_tier_node::EasyTierNode;

/// 中心主机名正则：`scaffolding-mc-server-{端口}`。
static HOSTNAME_PATTERN: OnceLock<Regex> = OnceLock::new();

fn hostname_pattern() -> &'static Regex {
    HOSTNAME_PATTERN.get_or_init(|| {
        Regex::new(r"^scaffolding-mc-server-(\d+)$").expect("编译中心主机名正则失败")
    })
}

/// 联机中心发现结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CenterDiscoveryResult {
    /// 中心虚拟 IP。
    pub virtual_ip: String,
    /// 中心服务端口。
    pub port: u16,
}

/// 从节点列表解析联机中心：主机名匹配正则且端口在 (1024, 65535] 范围内；无匹配返回 None。
pub fn try_parse_center(nodes: &[EasyTierNode]) -> Option<CenterDiscoveryResult> {
    for node in nodes {
        let Some(caps) = hostname_pattern().captures(&node.hostname) else {
            continue;
        };
        let Ok(port) = caps[1].parse::<u32>() else {
            continue;
        };
        if port > 1024 && port <= 65535 {
            return Some(CenterDiscoveryResult {
                virtual_ip: node.virtual_ip.clone(),
                port: port as u16,
            });
        }
    }
    None
}

/// 轮询扫描节点列表查找联机中心，最多 60 次 × 500ms（超时 30s）；未发现时返回 `CenterNotFound`。
pub async fn discover<F, Fut>(get_nodes: F) -> Result<CenterDiscoveryResult, ScaffoldingError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Vec<EasyTierNode>>,
{
    for attempt in 0..60 {
        let nodes = get_nodes().await;
        log::debug!(
            "第 {} 次扫描, 发现 {} 个节点: {}",
            attempt + 1,
            nodes.len(),
            nodes
                .iter()
                .map(|n| format!("{}/{}", n.virtual_ip, n.hostname))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(result) = try_parse_center(&nodes) {
            log::info!("发现联机中心: {}:{}", result.virtual_ip, result.port);
            return Ok(result);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(ScaffoldingError::CenterNotFound(
        "未在 EasyTier 网络中发现联机中心（超时 30s）".to_string(),
    ))
}
