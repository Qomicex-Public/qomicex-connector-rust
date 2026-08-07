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
