//! 心跳服务：按固定间隔调用回调发送心跳，发送失败或取消时停止。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::util::CancellationToken;

/// 心跳回调：返回 `Ok(())` 表示发送成功，`Err(())` 表示发送失败。
pub type HeartbeatCallback =
    Box<dyn FnMut() -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>> + Send>;

/// 心跳服务。
pub struct HeartbeatService {
    callback: HeartbeatCallback,
    on_failed: Option<Box<dyn Fn() + Send>>,
}

impl HeartbeatService {
    /// 创建心跳服务：`on_failed` 在心跳发送失败后调用一次。
    pub fn new(
        cb: impl FnMut() -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>> + Send + 'static,
        on_failed: Option<Box<dyn Fn() + Send>>,
    ) -> Self {
        Self {
            callback: Box::new(cb),
            on_failed,
        }
    }

    /// 启动心跳循环：按 `interval` 周期调用回调；回调返回 `Err` 时调用 `on_failed` 并停止，`ct` 取消时直接停止。
    pub async fn run(&mut self, interval: Duration, ct: CancellationToken) {
        log::info!("心跳服务已启动，间隔 {} 秒", interval.as_secs());
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    match (self.callback)().await {
                        Ok(()) => log::debug!("心跳发送成功"),
                        Err(()) => {
                            log::warn!("心跳发送异常，连接可能已断开");
                            if let Some(on_failed) = &self.on_failed {
                                on_failed();
                            }
                            break;
                        }
                    }
                }
                _ = ct.cancelled() => break,
            }
        }
    }
}
