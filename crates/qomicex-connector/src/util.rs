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
