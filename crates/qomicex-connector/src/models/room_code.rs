use std::sync::OnceLock;

use rand::Rng;
use regex::Regex;

use crate::error::ScaffoldingError;

/// 房间码模型：格式为 `U/XXXX-XXXX-XXXX-XXXX`，前两组为网络名，后两组为密钥。
pub struct RoomCode {
    raw: String,
    network_name_part: String,
    secret_part: String,
}

const PREFIX: &str = "U/";
const CHARS: &str = "0123456789ABCDEFGHJKLMNPQRSTUVWXYZ";

fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^U/([A-HJ-NP-Z0-9]{4})-([A-HJ-NP-Z0-9]{4})-([A-HJ-NP-Z0-9]{4})-([A-HJ-NP-Z0-9]{4})$")
            .expect("房间码正则表达式必须合法")
    })
}

impl RoomCode {
    fn new(raw: String, network_name_part: String, secret_part: String) -> Self {
        Self {
            raw,
            network_name_part,
            secret_part,
        }
    }

    /// 原始房间码，如 `U/G4J1-JZUE-TVUE-XBUB`。
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// 网络名部分（第 1-2 组）。
    pub fn network_name_part(&self) -> &str {
        &self.network_name_part
    }

    /// 密钥部分（第 3-4 组）。
    pub fn secret_part(&self) -> &str {
        &self.secret_part
    }

    /// EasyTier 网络名。
    pub fn easy_tier_network_name(&self) -> String {
        format!("scaffolding-mc-{}", self.network_name_part)
    }

    /// EasyTier 网络密钥。
    pub fn easy_tier_network_secret(&self) -> &str {
        &self.secret_part
    }

    /// 解析房间码；格式非法时返回错误。
    pub fn parse(code: &str) -> Result<Self, ScaffoldingError> {
        let caps = pattern()
            .captures(code)
            .ok_or_else(|| ScaffoldingError::RoomCodeInvalid(format!("无效的房间码格式: {code}")))?;

        let network_part = format!("{}-{}", &caps[1], &caps[2]);
        let secret_part = format!("{}-{}", &caps[3], &caps[4]);

        Ok(Self::new(code.to_string(), network_part, secret_part))
    }

    /// 生成随机房间码，校验和（mod 7）通过后返回。
    pub fn generate() -> Self {
        let mut buffer = [0u8; 8];
        let part1 = loop {
            rand::thread_rng().fill(&mut buffer);
            let part = encode8(&buffer);
            if validate_checksum(&part) {
                break part;
            }
        };
        let part2 = loop {
            rand::thread_rng().fill(&mut buffer);
            let part = encode8(&buffer);
            if validate_checksum(&part) {
                break part;
            }
        };

        let raw = format!("{PREFIX}{}-{}-{}-{}", &part1[..4], &part1[4..], &part2[..4], &part2[4..]);
        Self::parse(&raw).expect("生成的房间码必须合法")
    }
}

/// 8 字节按 `% 34` 映射为 8 个字符（base-34）。
fn encode8(bytes: &[u8; 8]) -> String {
    let mut chars = String::with_capacity(8);
    for &b in bytes {
        let idx = (b as usize) % CHARS.len();
        chars.push(CHARS.as_bytes()[idx] as char);
    }
    chars
}

/// 校验和：交替加减，结果被 7 整除则通过。
fn validate_checksum(eight_chars: &str) -> bool {
    let mut value: i64 = 0;
    for (i, c) in eight_chars.chars().enumerate() {
        let Some(digit) = CHARS.find(c) else {
            return false;
        };
        value += if i % 2 == 0 { digit as i64 } else { -(digit as i64) };
    }
    value % 7 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_CODE: &str = "U/G4J1-JZUE-TVUE-XBUB";

    #[test]
    fn parse_valid_code_returns_room_code() {
        let code = RoomCode::parse(VALID_CODE).expect("有效代码应解析成功");
        assert_eq!(code.raw(), VALID_CODE);
        assert_eq!(code.network_name_part(), "G4J1-JZUE");
        assert_eq!(code.secret_part(), "TVUE-XBUB");
        assert_eq!(code.easy_tier_network_name(), "scaffolding-mc-G4J1-JZUE");
        assert_eq!(code.easy_tier_network_secret(), "TVUE-XBUB");
    }

    #[test]
    fn parse_invalid_format_returns_error() {
        assert!(RoomCode::parse("bad").is_err());
        assert!(RoomCode::parse("U/12").is_err());
    }

    #[test]
    fn generate_produces_valid_code() {
        for _ in 0..100 {
            let code = RoomCode::generate();
            let reparsed = RoomCode::parse(code.raw()).expect("生成代码应可重新解析");
            assert_eq!(code.raw(), reparsed.raw());
        }
    }
}
