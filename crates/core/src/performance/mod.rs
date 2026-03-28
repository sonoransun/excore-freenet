//! Performance Optimization Infrastructure
//!
//! This module provides cutting-edge performance optimizations including:
//! - io_uring for high-performance async I/O on Linux
//! - SIMD acceleration for cryptographic operations
//! - Advanced memory management and zero-copy operations
//! - High-performance networking with batched operations

#[cfg(feature = "performance-opt")]
pub mod io_uring;

#[cfg(feature = "performance-opt")]
pub mod simd;

#[cfg(feature = "performance-opt")]
pub mod memory;

#[cfg(feature = "performance-opt")]
pub mod batching;

use std::sync::Arc;
use anyhow::Result;

// Re-export key types for easier access
#[cfg(feature = "performance-opt")]
pub use io_uring::{IoUringManager, IoUringSocket, IoUringStats, UdpSendOp};

#[cfg(feature = "performance-opt")]
pub use simd::{SimdCrypto, SimdStats};

#[cfg(feature = "performance-opt")]
pub use memory::{MemoryManager, MemoryStats, PooledBuffer, MappedFile};

#[cfg(feature = "performance-opt")]
pub use batching::{BatchManager, BatchStats, Priority};

/// Configuration for performance optimizations
#[derive(Clone, Debug)]
pub struct PerformanceConfig {
    /// Enable io_uring for async I/O (Linux only)
    pub enable_io_uring: bool,
    /// Enable SIMD acceleration for crypto
    pub enable_simd_crypto: bool,
    /// Enable zero-copy optimizations
    pub enable_zero_copy: bool,
    /// Enable batched network operations
    pub enable_batching: bool,
    /// io_uring queue depth
    pub io_uring_queue_depth: u32,
    /// Batch size for network operations
    pub network_batch_size: usize,
    /// Memory pool size for zero-copy buffers
    pub memory_pool_size: usize,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            enable_io_uring: cfg!(target_os = "linux"),
            enable_simd_crypto: true,
            enable_zero_copy: true,
            enable_batching: true,
            io_uring_queue_depth: 256,
            network_batch_size: 32,
            memory_pool_size: 1024 * 1024 * 16, // 16MB pool
        }
    }
}

/// Performance optimization manager
pub struct PerformanceManager {
    config: PerformanceConfig,

    #[cfg(feature = "performance-opt")]
    io_uring_manager: Option<Arc<io_uring::IoUringManager>>,

    #[cfg(feature = "performance-opt")]
    simd_crypto: Option<Arc<simd::SimdCrypto>>,

    #[cfg(feature = "performance-opt")]
    memory_manager: Option<Arc<memory::MemoryManager>>,

    #[cfg(feature = "performance-opt")]
    batch_manager: Option<Arc<batching::BatchManager>>,
}

impl PerformanceManager {
    /// Create new performance manager
    pub fn new(config: PerformanceConfig) -> Result<Self> {
        let mut manager = Self {
            config: config.clone(),

            #[cfg(feature = "performance-opt")]
            io_uring_manager: None,

            #[cfg(feature = "performance-opt")]
            simd_crypto: None,

            #[cfg(feature = "performance-opt")]
            memory_manager: None,

            #[cfg(feature = "performance-opt")]
            batch_manager: None,
        };

        manager.initialize()?;
        Ok(manager)
    }

    /// Initialize performance optimizations
    fn initialize(&mut self) -> Result<()> {
        #[cfg(feature = "performance-opt")]
        {
            // Initialize io_uring manager
            if self.config.enable_io_uring && cfg!(target_os = "linux") {
                self.io_uring_manager = Some(Arc::new(
                    io_uring::IoUringManager::new(self.config.io_uring_queue_depth)?
                ));
                tracing::info!("Initialized io_uring with queue depth {}", self.config.io_uring_queue_depth);
            }

            // Initialize SIMD crypto
            if self.config.enable_simd_crypto {
                self.simd_crypto = Some(Arc::new(simd::SimdCrypto::new()?));
                tracing::info!("Initialized SIMD crypto acceleration");
            }

            // Initialize memory manager
            if self.config.enable_zero_copy {
                self.memory_manager = Some(Arc::new(
                    memory::MemoryManager::new(self.config.memory_pool_size)?
                ));
                tracing::info!("Initialized memory manager with {} bytes pool", self.config.memory_pool_size);
            }

            // Initialize batch manager
            if self.config.enable_batching {
                self.batch_manager = Some(Arc::new(
                    batching::BatchManager::new(self.config.network_batch_size)?
                ));
                tracing::info!("Initialized batch manager with batch size {}", self.config.network_batch_size);
            }
        }

        Ok(())
    }

    /// Get io_uring manager if available
    #[cfg(feature = "performance-opt")]
    pub fn io_uring(&self) -> Option<Arc<io_uring::IoUringManager>> {
        self.io_uring_manager.clone()
    }

    /// Get SIMD crypto if available
    #[cfg(feature = "performance-opt")]
    pub fn simd_crypto(&self) -> Option<Arc<simd::SimdCrypto>> {
        self.simd_crypto.clone()
    }

    /// Get memory manager if available
    #[cfg(feature = "performance-opt")]
    pub fn memory_manager(&self) -> Option<Arc<memory::MemoryManager>> {
        self.memory_manager.clone()
    }

    /// Get batch manager if available
    #[cfg(feature = "performance-opt")]
    pub fn batch_manager(&self) -> Option<Arc<batching::BatchManager>> {
        self.batch_manager.clone()
    }

    /// Get performance statistics
    pub fn get_stats(&self) -> PerformanceStats {
        let mut stats = PerformanceStats::default();

        #[cfg(feature = "performance-opt")]
        {
            if let Some(ref io_uring) = self.io_uring_manager {
                stats.io_uring_stats = Some(io_uring.get_stats());
            }

            if let Some(ref simd) = self.simd_crypto {
                stats.simd_stats = Some(simd.get_stats());
            }

            if let Some(ref memory) = self.memory_manager {
                stats.memory_stats = Some(memory.get_stats());
            }

            if let Some(ref batch) = self.batch_manager {
                stats.batch_stats = Some(batch.get_stats());
            }
        }

        stats
    }
}

/// Performance statistics
#[derive(Debug, Default)]
pub struct PerformanceStats {
    #[cfg(feature = "performance-opt")]
    pub io_uring_stats: Option<io_uring::IoUringStats>,

    #[cfg(feature = "performance-opt")]
    pub simd_stats: Option<simd::SimdStats>,

    #[cfg(feature = "performance-opt")]
    pub memory_stats: Option<memory::MemoryStats>,

    #[cfg(feature = "performance-opt")]
    pub batch_stats: Option<batching::BatchStats>,
}

/// Enhanced component wrapper for performance optimizations
pub struct PerformanceEnhanced<T> {
    /// Base implementation
    pub base: T,
    /// Performance manager
    pub perf_manager: Arc<PerformanceManager>,
    /// Whether performance optimizations are enabled
    pub enabled: bool,
}

impl<T> PerformanceEnhanced<T> {
    /// Create new performance-enhanced component
    pub fn new(base: T, perf_manager: Arc<PerformanceManager>) -> Self {
        Self {
            base,
            perf_manager: perf_manager.clone(),
            enabled: true,
        }
    }

    /// Create component without performance optimizations
    pub fn fallback_only(base: T) -> Self {
        let config = PerformanceConfig {
            enable_io_uring: false,
            enable_simd_crypto: false,
            enable_zero_copy: false,
            enable_batching: false,
            ..Default::default()
        };

        let perf_manager = Arc::new(
            PerformanceManager::new(config)
                .unwrap_or_else(|_| panic!("Failed to create fallback performance manager"))
        );

        Self {
            base,
            perf_manager,
            enabled: false,
        }
    }

    /// Check if performance optimizations are available and enabled
    pub fn has_optimizations(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_performance_config_default() {
        let config = PerformanceConfig::default();
        assert_eq!(config.io_uring_queue_depth, 256);
        assert_eq!(config.network_batch_size, 32);
        assert_eq!(config.memory_pool_size, 1024 * 1024 * 16);

        // Platform-specific defaults
        #[cfg(target_os = "linux")]
        assert!(config.enable_io_uring);

        #[cfg(not(target_os = "linux"))]
        assert!(!config.enable_io_uring);
    }

    #[test]
    fn test_performance_manager_creation() {
        let config = PerformanceConfig::default();
        let result = PerformanceManager::new(config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_performance_enhanced_fallback() {
        let base_value = 42u32;
        let enhanced = PerformanceEnhanced::fallback_only(base_value);
        assert_eq!(enhanced.base, 42);
        assert!(!enhanced.has_optimizations());
    }

    #[test]
    fn test_performance_stats_default() {
        let stats = PerformanceStats::default();
        // Should compile and create default stats
        // Actual fields depend on feature flags
        let _stats = stats;
    }
}