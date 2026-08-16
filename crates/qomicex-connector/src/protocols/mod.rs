//! 协议处理器层（对应 C# `Protocols/IProtocol.cs`）。
//!
//! 提供 [`ProtocolHandler`] trait 与 5 个标准协议
//! （`c:ping` / `c:protocols` / `c:server_port` / `c:player_ping` / `c:player_profiles_list`），
//! 以及用于快速注册自定义扩展协议的 [`DelegateProtocol`]。

use std::future::Future;
use std::pin::Pin;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::models::player::{PlayerInfo, PlayerKind, PlayerProfileEntry};
use crate::models::protocol::{ProtocolRequest, ProtocolResponse};

/// 协议处理器（对应 C# `IProtocol`）。
///
/// 所有标准协议均为同步计算，`handle` 统一以 `Box::pin(async move { ... })` 包装。
pub trait ProtocolHandler: Send + Sync {
    /// 协议键（如 `"c:ping"`）。
    fn key(&self) -> &str;
    /// 处理请求，返回响应。
    fn handle<'a>(
        &'a self,
        request: &'a ProtocolRequest,
    ) -> Pin<Box<dyn Future<Output = ProtocolResponse> + Send + 'a>>;
}

/// 构造失败响应（status 255 + UTF8 错误消息，与"未知协议 → status 255"约定一致）。
fn error_response(message: impl Into<String>) -> ProtocolResponse {
    ProtocolResponse {
        status: 255,
        body: message.into().into_bytes(),
    }
}

/// 协议 `c:ping`：原样回显请求体。
#[derive(Debug, Clone, Copy, Default)]
pub struct PingProtocol;

impl ProtocolHandler for PingProtocol {
    fn key(&self) -> &str {
        "c:ping"
    }

    fn handle<'a>(
        &'a self,
        request: &'a ProtocolRequest,
    ) -> Pin<Box<dyn Future<Output = ProtocolResponse> + Send + 'a>> {
        Box::pin(async move {
            ProtocolResponse {
                status: 0,
                body: request.body.clone(),
            }
        })
    }
}

/// 协议 `c:protocols`：返回支持的协议列表（`\0` 分隔的 UTF8 字节）。
#[derive(Debug, Clone)]
pub struct ProtocolsProtocol {
    supported: Vec<String>,
}

impl ProtocolsProtocol {
    /// 以支持协议列表构造（对应 C# `ProtocolsProtocol(IReadOnlyList<string>)`）。
    pub fn new(supported_protocols: Vec<String>) -> Self {
        Self {
            supported: supported_protocols,
        }
    }
}

impl ProtocolHandler for ProtocolsProtocol {
    fn key(&self) -> &str {
        "c:protocols"
    }

    fn handle<'a>(
        &'a self,
        _request: &'a ProtocolRequest,
    ) -> Pin<Box<dyn Future<Output = ProtocolResponse> + Send + 'a>> {
        Box::pin(async move {
            ProtocolResponse {
                status: 0,
                body: self.supported.join("\0").into_bytes(),
            }
        })
    }
}

/// 协议 `c:server_port`：返回大端 u16 端口（2 字节）。
#[derive(Debug, Clone, Copy)]
pub struct ServerPortProtocol {
    port: u16,
}

impl ServerPortProtocol {
    /// 以端口号构造。
    pub fn new(port: u16) -> Self {
        Self { port }
    }
}

impl ProtocolHandler for ServerPortProtocol {
    fn key(&self) -> &str {
        "c:server_port"
    }

    fn handle<'a>(
        &'a self,
        _request: &'a ProtocolRequest,
    ) -> Pin<Box<dyn Future<Output = ProtocolResponse> + Send + 'a>> {
        Box::pin(async move {
            ProtocolResponse {
                status: 0,
                body: self.port.to_be_bytes().to_vec(),
            }
        })
    }
}

/// 协议 `c:player_ping`：解析玩家心跳 JSON 并触发回调。
///
/// 回调返回 `true` = 接受该玩家（响应状态 0）；返回 `false` = 拒绝（如已被房主踢出，
/// 响应状态 255 且**不刷新心跳**——被拒客户端仍会被 15s 心跳窗口剔除，双重兜底）。
pub struct PlayerPingProtocol {
    on_player_ping: Box<dyn Fn(PlayerInfo) -> bool + Send + Sync>,
}

impl PlayerPingProtocol {
    /// 以玩家回调构造（对应 C# `PlayerPingProtocol(Action<PlayerInfo>)`）。
    pub fn new(on_player_ping: impl Fn(PlayerInfo) -> bool + Send + Sync + 'static) -> Self {
        Self {
            on_player_ping: Box::new(on_player_ping),
        }
    }
}

impl ProtocolHandler for PlayerPingProtocol {
    fn key(&self) -> &str {
        "c:player_ping"
    }

    fn handle<'a>(
        &'a self,
        request: &'a ProtocolRequest,
    ) -> Pin<Box<dyn Future<Output = ProtocolResponse> + Send + 'a>> {
        Box::pin(async move {
            match parse_player_info(&request.body) {
                Ok(info) => {
                    if (self.on_player_ping)(info) {
                        ProtocolResponse {
                            status: 0,
                            body: Vec::new(),
                        }
                    } else {
                        ProtocolResponse {
                            status: 255,
                            body: "玩家已被房主踢出".as_bytes().to_vec(),
                        }
                    }
                }
                Err(e) => error_response(format!("玩家心跳解析失败: {e}")),
            }
        })
    }
}

/// 宽容解析 JSON 值为字符串（跨启动器互操作）：
/// 字符串原样返回；数字（int/uint/float）转字符串——第三方启动器可能发
/// 数字类型的 `easytier_id`/`machine_id`（C# `GetString()` 会抛异常、
/// `Value::as_str` 返回 None，本实现容忍并归一为字符串）。
pub(crate) fn value_to_string(v: &Value) -> Option<String> {
    v.as_str()
        .map(String::from)
        .or_else(|| v.as_i64().map(|n| n.to_string()))
        .or_else(|| v.as_u64().map(|n| n.to_string()))
        .or_else(|| v.as_f64().map(|n| n.to_string()))
}

/// 手动解析玩家心跳 JSON。
///
/// 容错语义为**有意改进**（对比 C# `GetProperty().GetString()` 裸调用）：
/// C# 在缺失属性 / 非字符串 / JSON 非法时抛异常导致整个连接被断开且不写响应；
/// 本实现缺失属性回退空串、解析失败返回 status 255（保持连接，
/// 且 255 不刷新心跳 → 畸形客户端仍会在 15s 心跳窗口后被剔除）。
fn parse_player_info(body: &[u8]) -> Result<PlayerInfo, serde_json::Error> {
    let root: Value = serde_json::from_slice(body)?;
    let str_or = |key: &str| {
        root.get(key)
            .and_then(value_to_string)
            .unwrap_or_default()
    };
    Ok(PlayerInfo {
        name: str_or("name"),
        machine_id: str_or("machine_id"),
        vendor: str_or("vendor"),
        easytier_id: root.get("easytier_id").and_then(value_to_string),
        kind: PlayerKind::Host,
    })
}

/// 协议 `c:player_profiles_list`：返回玩家列表（JSON 数组，snake_case，kind 为 "HOST"/"GUEST"）。
pub struct PlayerProfilesListProtocol {
    get_players: Box<dyn Fn() -> Vec<PlayerInfo> + Send + Sync>,
}

impl PlayerProfilesListProtocol {
    /// 以获取玩家列表的回调构造（对应 C# `PlayerProfilesListProtocol(Func<IReadOnlyList<PlayerInfo>>)`）。
    pub fn new(get_players: impl Fn() -> Vec<PlayerInfo> + Send + Sync + 'static) -> Self {
        Self {
            get_players: Box::new(get_players),
        }
    }
}

impl ProtocolHandler for PlayerProfilesListProtocol {
    fn key(&self) -> &str {
        "c:player_profiles_list"
    }

    fn handle<'a>(
        &'a self,
        _request: &'a ProtocolRequest,
    ) -> Pin<Box<dyn Future<Output = ProtocolResponse> + Send + 'a>> {
        Box::pin(async move {
            let entries: Vec<PlayerProfileEntry> = (self.get_players)()
                .into_iter()
                .map(player_to_entry)
                .collect();
            match serde_json::to_vec(&entries) {
                Ok(body) => ProtocolResponse { status: 0, body },
                Err(e) => error_response(format!("玩家列表序列化失败: {e}")),
            }
        })
    }
}

/// `PlayerInfo` → `PlayerProfileEntry`（对应 C# 匿名映射，kind 为 "HOST"/"GUEST" 字符串）。
fn player_to_entry(p: PlayerInfo) -> PlayerProfileEntry {
    PlayerProfileEntry {
        name: p.name,
        machine_id: p.machine_id,
        easytier_id: p.easytier_id,
        vendor: p.vendor,
        kind: Some(match p.kind {
            PlayerKind::Host => "HOST".to_string(),
            PlayerKind::Guest => "GUEST".to_string(),
        }),
    }
}

/// 通用委托协议适配器（对应 C# `DelegateProtocol` 系列），用于快速注册自定义扩展协议（如 `qml:game_info`）。
pub struct DelegateProtocol {
    key: String,
    handler: Box<dyn Fn(&ProtocolRequest) -> ProtocolResponse + Send + Sync>,
}

impl DelegateProtocol {
    /// 原始字节版（对应 C# `DelegateProtocol(string, Func<byte[], byte[]>)`）：status 恒为 0。
    pub fn new_raw(
        key: impl Into<String>,
        handler: impl Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
    ) -> Self {
        Self {
            key: key.into(),
            handler: Box::new(move |req| ProtocolResponse {
                status: 0,
                body: handler(&req.body),
            }),
        }
    }

    /// 完整版（对应 C# `DelegateProtocol(string, Func<ProtocolRequest, CancellationToken, Task<ProtocolResponse>>)`）。
    pub fn new(
        key: impl Into<String>,
        handler: Box<dyn Fn(&ProtocolRequest) -> ProtocolResponse + Send + Sync>,
    ) -> Self {
        Self {
            key: key.into(),
            handler,
        }
    }

    /// 无入参、JSON 响应版（对应 C# `DelegateProtocol<TResp>`）：handler 返回值序列化为响应体。
    pub fn new_json<TResp: Serialize>(
        key: impl Into<String>,
        handler: impl Fn() -> TResp + Send + Sync + 'static,
    ) -> Self {
        Self {
            key: key.into(),
            handler: Box::new(move |_req| match serde_json::to_vec(&handler()) {
                Ok(body) => ProtocolResponse { status: 0, body },
                Err(e) => error_response(format!("响应序列化失败: {e}")),
            }),
        }
    }

    /// 带入参、JSON 响应版（对应 C# `DelegateProtocol<TReq, TResp>`）：
    /// 请求体反序列化为 `TReq`，空请求体 → `Default`；返回值序列化为响应体。
    pub fn new_json_req<TReq, TResp>(
        key: impl Into<String>,
        handler: impl Fn(TReq) -> TResp + Send + Sync + 'static,
    ) -> Self
    where
        TReq: DeserializeOwned + Default,
        TResp: Serialize,
    {
        Self {
            key: key.into(),
            handler: Box::new(move |req| {
                let arg = if req.body.is_empty() {
                    TReq::default()
                } else {
                    match serde_json::from_slice::<TReq>(&req.body) {
                        Ok(arg) => arg,
                        Err(e) => return error_response(format!("请求反序列化失败: {e}")),
                    }
                };
                match serde_json::to_vec(&handler(arg)) {
                    Ok(body) => ProtocolResponse { status: 0, body },
                    Err(e) => error_response(format!("响应序列化失败: {e}")),
                }
            }),
        }
    }
}

impl ProtocolHandler for DelegateProtocol {
    fn key(&self) -> &str {
        &self.key
    }

    fn handle<'a>(
        &'a self,
        request: &'a ProtocolRequest,
    ) -> Pin<Box<dyn Future<Output = ProtocolResponse> + Send + 'a>> {
        Box::pin(async move { (self.handler)(request) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn ping_protocol_echoes_body() {
        let handler = PingProtocol;
        let body = vec![0x01, 0x02, 0x03];
        let response = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "ping".into(),
                body: body.clone(),
            })
            .await;

        assert_eq!(response.status, 0);
        assert_eq!(response.body, body);
    }

    #[tokio::test]
    async fn protocols_protocol_returns_supported_protocols() {
        let handler = ProtocolsProtocol::new(vec![
            "c:ping".into(),
            "c:protocols".into(),
            "c:server_port".into(),
        ]);
        let response = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "protocols".into(),
                body: Vec::new(),
            })
            .await;

        assert_eq!(response.status, 0);
        let text = String::from_utf8(response.body).expect("响应体应为 UTF8");
        let protocols: Vec<&str> = text.split('\0').collect();
        assert!(protocols.contains(&"c:ping"));
        assert!(protocols.contains(&"c:protocols"));
        assert!(protocols.contains(&"c:server_port"));
    }

    #[tokio::test]
    async fn server_port_protocol_returns_port() {
        let handler = ServerPortProtocol::new(25565);
        let response = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "server_port".into(),
                body: Vec::new(),
            })
            .await;

        assert_eq!(response.status, 0);
        assert_eq!(response.body.len(), 2);
        let port = u16::from_be_bytes([response.body[0], response.body[1]]);
        assert_eq!(port, 25565);
    }

    #[tokio::test]
    async fn player_ping_protocol_parses_json_and_invokes_callback() {
        let captured = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let handler = PlayerPingProtocol::new(move |info| {
            *captured_clone.lock().unwrap() = Some(info);
            true
        });

        let json = json!({
            "name": "TestPlayer",
            "machine_id": "m123",
            "vendor": "TestVendor"
        });
        let response = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_ping".into(),
                body: serde_json::to_vec(&json).expect("构造测试体失败"),
            })
            .await;

        assert_eq!(response.status, 0);
        let info = captured
            .lock()
            .unwrap()
            .clone()
            .expect("回调未触发");
        assert_eq!(info.name, "TestPlayer");
        assert_eq!(info.machine_id, "m123");
        assert_eq!(info.vendor, "TestVendor");
        assert_eq!(info.easytier_id, None);
    }

    #[tokio::test]
    async fn player_ping_parses_numeric_easytier_id_and_machine_id() {
        // 第三方启动器可能发数字类型的 easytier_id/machine_id：
        // 必须归一为字符串（C# GetString() 会抛异常、as_str 返回 None → bug）。
        let captured = Arc::new(Mutex::new(None));
        let captured_clone = captured.clone();
        let handler = PlayerPingProtocol::new(move |info| {
            *captured_clone.lock().unwrap() = Some(info);
            true
        });

        let json = json!({
            "name": "NumericThirdParty",
            "machine_id": 9527,
            "vendor": "third-party",
            "easytier_id": 123456
        });
        let response = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_ping".into(),
                body: serde_json::to_vec(&json).expect("构造测试体失败"),
            })
            .await;

        assert_eq!(response.status, 0);
        let info = captured
            .lock()
            .unwrap()
            .clone()
            .expect("回调未触发");
        assert_eq!(info.machine_id, "9527");
        assert_eq!(info.easytier_id.as_deref(), Some("123456"));
    }

    #[tokio::test]
    async fn player_ping_protocol_rejected_callback_returns_error_status() {
        // 已被房主踢出的玩家：回调返回 false → 状态 255（且不刷新心跳，兜底剔除）
        let handler = PlayerPingProtocol::new(|_| false);
        let json = json!({
            "name": "KickedPlayer",
            "machine_id": "k1",
            "vendor": "TestVendor"
        });
        let response = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_ping".into(),
                body: serde_json::to_vec(&json).expect("构造测试体失败"),
            })
            .await;

        assert_eq!(response.status, 255);
    }

    #[tokio::test]
    async fn player_profiles_list_protocol_returns_player_list() {
        let players = vec![
            PlayerInfo {
                name: "Host".into(),
                machine_id: "h1".into(),
                vendor: "V".into(),
                easytier_id: None,
                kind: PlayerKind::Host,
            },
            PlayerInfo {
                name: "Guest1".into(),
                machine_id: "g1".into(),
                vendor: "V".into(),
                easytier_id: None,
                kind: PlayerKind::Guest,
            },
        ];
        let handler = PlayerProfilesListProtocol::new(move || players.clone());
        let response = handler
            .handle(&ProtocolRequest {
                namespace: "c".into(),
                request_type: "player_profiles_list".into(),
                body: Vec::new(),
            })
            .await;

        assert_eq!(response.status, 0);
        let entries: Vec<PlayerProfileEntry> =
            serde_json::from_slice(&response.body).expect("响应体应为 JSON 数组");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Host");
        assert_eq!(entries[0].kind.as_deref(), Some("HOST"));
        assert_eq!(entries[1].name, "Guest1");
        assert_eq!(entries[1].kind.as_deref(), Some("GUEST"));
    }
}
