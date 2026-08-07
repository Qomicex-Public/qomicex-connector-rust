//! 中继节点提供器：从远程节点服务获取中继节点，失败时回退到内置默认节点。

use std::time::Duration;

use serde_json::Value;

use super::nodes::DEFAULT_NODES;

/// 节点服务端点。
pub const ENDPOINT: &str = "https://nodes.qomicex.top/api/nodes";

/// 默认 User-Agent。
pub const DEFAULT_USER_AGENT: &str = "Qomicex.Connector/1.0";

/// 检测系统地区：取系统语言（如 "zh-CN"）中 "-" 后段（"CN"），失败回退 "CN"。
fn detect_system_region() -> String {
    match sys_locale::get_locale() {
        Some(locale) => locale
            .split('-')
            .nth(1)
            .map(str::to_ascii_uppercase)
            .unwrap_or_else(|| "CN".to_string()),
        None => "CN".to_string(),
    }
}

/// 判断节点是否为 http(s) 节点（大小写不敏感）。
fn is_http_url(node: &str) -> bool {
    node.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
        || node
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
}

/// 内置默认节点列表。
fn default_nodes() -> Vec<String> {
    DEFAULT_NODES.iter().map(|s| s.to_string()).collect()
}

/// 中继节点提供器。
pub struct RelayNodeProvider {
    user_agent: String,
    preferred_region: Option<String>,
    client: reqwest::Client,
}

impl RelayNodeProvider {
    /// 创建提供器：user_agent 为空时使用默认值，preferred_region 未指定时自动检测系统地区。
    pub fn new(user_agent: Option<String>, preferred_region: Option<String>) -> Self {
        let user_agent = user_agent
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());
        let preferred_region = preferred_region.or_else(|| Some(detect_system_region()));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .no_proxy()
            .build()
            .expect("构建 HTTP 客户端失败");
        Self {
            user_agent,
            preferred_region,
            client,
        }
    }

    /// 从默认端点获取中继节点列表。
    pub async fn fetch(&self) -> Vec<String> {
        self.fetch_from(ENDPOINT).await
    }

    /// 从指定端点获取中继节点列表（内部可注入实现，便于测试）。
    pub async fn fetch_from(&self, url: &str) -> Vec<String> {
        match self.fetch_nodes(url).await {
            Some(nodes) if nodes.is_empty() => {
                log::warn!("节点服务返回空列表，回退到内置默认节点");
                default_nodes()
            }
            Some(nodes) => {
                log::info!("已从节点服务获取 {} 个中继节点", nodes.len());
                nodes
            }
            None => {
                log::warn!("获取中继节点失败，回退到内置默认节点");
                default_nodes()
            }
        }
    }

    /// 请求端点并解析节点：先解析 JSON，再将 http(s) 节点二次解析为实际节点。
    async fn fetch_nodes(&self, url: &str) -> Option<Vec<String>> {
        let response = self
            .client
            .get(url)
            .header("User-Agent", self.user_agent.as_str())
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let json = response.text().await.ok()?;
        let nodes = parse_nodes(&json, self.preferred_region.as_deref());
        let mut resolved = Vec::with_capacity(nodes.len());
        for node in nodes {
            if is_http_url(&node) {
                match self.resolve_http_node(&node).await {
                    Some(actual) => resolved.push(actual),
                    None => log::warn!("解析节点 {node} 失败，已跳过"),
                }
            } else {
                resolved.push(node);
            }
        }
        Some(resolved)
    }

    /// 请求 http(s) 节点地址，返回其返回体 trim 后的实际节点；失败返回 None。
    async fn resolve_http_node(&self, node_url: &str) -> Option<String> {
        let response = self
            .client
            .get(node_url)
            .header("User-Agent", self.user_agent.as_str())
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = response.text().await.ok()?;
        let trimmed = body.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

/// 解析节点 JSON 数组：元素需含非空 url 字符串与可选 region；preferred 匹配（大小写不敏感）排前，其余保序在后。
/// JSON 非法或非数组时返回空列表。
pub fn parse_nodes(json: &str, preferred_region: Option<&str>) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    let mut preferred = Vec::new();
    let mut rest = Vec::new();
    for element in array {
        let Some(url) = element.get("url").and_then(|u| u.as_str()) else {
            continue;
        };
        if url.trim().is_empty() {
            continue;
        }
        let region = element.get("region").and_then(|r| r.as_str());
        let is_preferred = preferred_region
            .is_some_and(|pref| region.is_some_and(|r| r.eq_ignore_ascii_case(pref)));
        (if is_preferred { &mut preferred } else { &mut rest }).push(url.to_string());
    }
    preferred.extend(rest);
    preferred
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    use super::*;

    /// 构造 HTTP/1.1 响应文本。
    fn http_response(status_line: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// 启动本地 HTTP 服务器：记录收到的请求头，对每个连接返回固定响应。
    async fn start_server(response: String) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_loop = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    continue;
                };
                let seen = seen_loop.clone();
                let response = response.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let Ok(n) = stream.read(&mut chunk).await else {
                            break;
                        };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            seen.lock()
                                .await
                                .push(String::from_utf8_lossy(&buf).into_owned());
                            let _ = stream.write_all(response.as_bytes()).await;
                            break;
                        }
                    }
                });
            }
        });
        (addr, seen)
    }

    #[test]
    fn parse_nodes_valid_object_array_returns_urls() {
        let json = r#"[{"url":"tcp://a.example.com:11010","name":"hk"},{"url":"udp://b.example.com:11010","region":"jp"}]"#;
        let nodes = parse_nodes(json, None);
        assert_eq!(
            nodes,
            ["tcp://a.example.com:11010", "udp://b.example.com:11010"]
        );
    }

    #[test]
    fn parse_nodes_missing_or_empty_url_skipped() {
        let json = r#"[{"name":"no-url"},{"url":""},{"url":"tcp://ok.example.com:11010"}]"#;
        let nodes = parse_nodes(json, None);
        assert_eq!(nodes, ["tcp://ok.example.com:11010"]);
    }

    #[test]
    fn parse_nodes_invalid_json_returns_empty() {
        assert!(parse_nodes("not json", None).is_empty());
    }

    #[test]
    fn parse_nodes_not_array_returns_empty() {
        assert!(parse_nodes(r#"{"url":"tcp://a:11010"}"#, None).is_empty());
    }

    #[test]
    fn parse_nodes_preferred_region_matching_nodes_first() {
        let json = r#"[
          {"url":"https://node-jp.example.com","region":"JP"},
          {"url":"https://node-cn1.example.com","region":"CN"},
          {"url":"https://node-none.example.com"},
          {"url":"https://node-cn2.example.com","region":"CN"}
        ]"#;
        let nodes = parse_nodes(json, Some("CN"));
        assert_eq!(
            nodes,
            [
                "https://node-cn1.example.com",
                "https://node-cn2.example.com",
                "https://node-jp.example.com",
                "https://node-none.example.com"
            ]
        );
    }

    #[test]
    fn parse_nodes_preferred_region_case_insensitive() {
        let json = r#"[{"url":"https://a.example.com","region":"JP"},{"url":"https://b.example.com","region":"cn"}]"#;
        let nodes = parse_nodes(json, Some("CN"));
        assert_eq!(nodes, ["https://b.example.com", "https://a.example.com"]);
    }

    #[test]
    fn parse_nodes_no_preferred_region_keeps_api_order() {
        let json = r#"[{"url":"https://a.example.com","region":"JP"},{"url":"https://b.example.com","region":"CN"}]"#;
        let nodes = parse_nodes(json, None);
        assert_eq!(
            nodes,
            ["https://a.example.com", "https://b.example.com"]
        );
    }

    #[test]
    fn parse_nodes_no_matching_region_keeps_api_order() {
        let json = r#"[{"url":"https://a.example.com","region":"JP"},{"url":"https://b.example.com","region":"US"}]"#;
        let nodes = parse_nodes(json, Some("CN"));
        assert_eq!(
            nodes,
            ["https://a.example.com", "https://b.example.com"]
        );
    }

    #[tokio::test]
    async fn fetch_valid_response_returns_fetched_nodes() {
        let (addr, _seen) = start_server(http_response(
            "200 OK",
            r#"[{"url":"tcp://a.example.com:11010"}]"#,
        ))
        .await;
        let provider = RelayNodeProvider::new(None, None);
        let nodes = provider.fetch_from(&format!("http://{addr}/nodes")).await;
        assert_eq!(nodes, ["tcp://a.example.com:11010"]);
    }

    #[tokio::test]
    async fn fetch_http_error_returns_default_nodes() {
        let (addr, _seen) = start_server(http_response("500 Internal Server Error", "")).await;
        let provider = RelayNodeProvider::new(None, None);
        let nodes = provider.fetch_from(&format!("http://{addr}/nodes")).await;
        assert_eq!(nodes, default_nodes());
    }

    #[tokio::test]
    async fn fetch_empty_array_returns_default_nodes() {
        let (addr, _seen) = start_server(http_response("200 OK", "[]")).await;
        let provider = RelayNodeProvider::new(None, None);
        let nodes = provider.fetch_from(&format!("http://{addr}/nodes")).await;
        assert_eq!(nodes, default_nodes());
    }

    #[tokio::test]
    async fn fetch_sends_custom_user_agent() {
        let (addr, seen) = start_server(http_response("200 OK", "[]")).await;
        let provider = RelayNodeProvider::new(Some("MyLauncher/2.3".to_string()), None);
        let _ = provider.fetch_from(&format!("http://{addr}/nodes")).await;
        let lock = seen.lock().await;
        let head = lock.first().expect("应收到请求");
        let lower = head.to_lowercase();
        assert!(
            lower.contains("user-agent: mylauncher/2.3"),
            "实际请求头: {head}"
        );
    }

    #[tokio::test]
    async fn fetch_default_user_agent_when_not_specified() {
        let (addr, seen) = start_server(http_response("200 OK", "[]")).await;
        let provider = RelayNodeProvider::new(None, None);
        let _ = provider.fetch_from(&format!("http://{addr}/nodes")).await;
        let lock = seen.lock().await;
        let head = lock.first().expect("应收到请求");
        let lower = head.to_lowercase();
        assert!(
            lower.contains("user-agent: qomicex.connector/1.0"),
            "实际请求头: {head}"
        );
    }

    #[tokio::test]
    async fn fetch_requests_configured_endpoint() {
        let (addr, seen) = start_server(http_response("200 OK", "[]")).await;
        let provider = RelayNodeProvider::new(None, None);
        let _ = provider.fetch_from(&format!("http://{addr}/nodes")).await;
        let lock = seen.lock().await;
        let head = lock.first().expect("应收到请求");
        let line = head.lines().next().unwrap_or_default();
        assert_eq!(line, "GET /nodes HTTP/1.1");
        assert_eq!(ENDPOINT, "https://nodes.qomicex.top/api/nodes");
    }
}
