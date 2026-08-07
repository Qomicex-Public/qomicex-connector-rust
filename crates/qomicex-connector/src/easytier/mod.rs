//! EasyTier 集成模块：基于 easytier 库（非进程）的虚拟网络管理。
//!
//! 替代 C# 版本中 spawn `easytier-core` 进程的方案，直接在进程内
//! 运行 EasyTier 网络实例，由 [`EasyTierManager`] 管理实例生命周期。

mod manager;

pub use manager::EasyTierManager;
