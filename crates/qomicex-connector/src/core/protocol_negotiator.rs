//! 协议协商：取我方与中心端共有的协议列表。

use std::collections::HashSet;

/// 协商共有协议：返回 `my` 中存在于 `center` 的项（保持 `my` 的顺序）。
pub fn negotiate(my: &[String], center: &[String]) -> Vec<String> {
    let center_set: HashSet<&str> = center.iter().map(String::as_str).collect();
    my.iter()
        .filter(|p| center_set.contains(p.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn negotiate_returns_intersection() {
        let my = strings(&[
            "c:ping",
            "c:protocols",
            "c:server_port",
            "c:player_ping",
            "c:player_profiles_list",
            "custom:extra",
        ]);
        let center = strings(&["c:ping", "c:protocols", "c:server_port", "c:player_ping"]);

        let result = negotiate(&my, &center);

        assert_eq!(result.len(), 4);
        assert!(result.contains(&"c:ping".to_string()));
        assert!(result.contains(&"c:protocols".to_string()));
        assert!(result.contains(&"c:server_port".to_string()));
        assert!(result.contains(&"c:player_ping".to_string()));
        assert!(!result.contains(&"c:player_profiles_list".to_string()));
        assert!(!result.contains(&"custom:extra".to_string()));
    }

    #[test]
    fn negotiate_empty_inputs_returns_empty() {
        assert!(negotiate(&[], &[]).is_empty());
    }
}
