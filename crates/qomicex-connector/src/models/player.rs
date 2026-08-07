//! 玩家信息与资料条目。

use serde::{Deserialize, Serialize};

/// 玩家角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    /// 房主。
    Host,
    /// 访客。
    Guest,
}

/// C# 枚举默认值为首个成员 Host。
impl Default for PlayerKind {
    fn default() -> Self {
        Self::Host
    }
}

/// 玩家信息（进程内使用）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerInfo {
    /// 玩家名。
    pub name: String,
    /// 机器标识。
    pub machine_id: String,
    /// EasyTier 节点 ID。
    pub easytier_id: Option<String>,
    /// 启动器厂商。
    pub vendor: String,
    /// 角色（Host / Guest）。
    pub kind: PlayerKind,
}

/// 玩家资料条目（JSON 通信格式，snake_case）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PlayerProfileEntry {
    /// 玩家名。
    pub name: String,
    /// 机器标识。
    pub machine_id: String,
    /// EasyTier 节点 ID（Guest 无时为 null，与 C# 一致）。
    pub easytier_id: Option<String>,
    /// 启动器厂商。
    pub vendor: String,
    /// 角色："HOST" / "GUEST"。
    pub kind: Option<String>,
}
