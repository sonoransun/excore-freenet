//! AI/ML Infrastructure for Freenet Core
//!
//! This module provides machine learning capabilities for network optimization,
//! intelligent routing, and anomaly detection. It supports multiple ML backends
//! including Candle (pure Rust) and ONNX Runtime for inference.
//!
//! ## Features
//!
//! - **ML-Enhanced Router**: Replace isotonic regression with neural networks
//! - **Adaptive Congestion Control**: Reinforcement learning for BBR/LEDBAT++ optimization
//! - **Contract Anomaly Detection**: ML-based security monitoring
//! - **Feature Engineering**: Extract features from transport metrics and routing history
//!
//! ## Architecture
//!
//! The ML infrastructure follows the enhanced component pattern:
//!
//! ```rust
//! pub struct Enhanced<T, E> {
//!     base: T,                    // Existing implementation
//!     enhancement: Option<E>,     // ML capability
//!     config: EnhancementConfig,  // Feature flags and fallback
//! }
//! ```
//!
//! This ensures backward compatibility and graceful degradation when ML models
//! are unavailable or fail.

#[cfg(feature = "ml-routing")]
pub mod routing;

#[cfg(feature = "rl-congestion")]
pub mod congestion;

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
pub mod backend;

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
pub mod features;

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
pub mod models;

// Re-export key types for easier access
#[cfg(feature = "ml-routing")]
pub use routing::{MLRouter, OperationType, RoutePrediction};

#[cfg(feature = "rl-congestion")]
pub use congestion::{RLCongestionController, CongestionAction, CongestionState};

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
pub use backend::{BackendManager, CandleBackend, OnnxBackend, FallbackBackend};

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
pub use features::{RoutingFeatureExtractor, CongestionFeatureExtractor};

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
pub use models::{ModelFactory, ModelConfig, ModelType};

use std::sync::Arc;
use anyhow::Result;

/// Configuration for ML enhancements
#[derive(Clone, Debug)]
pub struct MLConfig {
    /// Whether to enable ML enhancements
    pub enabled: bool,
    /// Whether to fall back to classical algorithms on ML failure
    pub fallback_on_error: bool,
    /// Path to store/load trained models
    pub model_path: Option<std::path::PathBuf>,
    /// Maximum inference latency before fallback (milliseconds)
    pub max_inference_latency_ms: u64,
}

impl Default for MLConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fallback_on_error: true,
            model_path: None,
            max_inference_latency_ms: 100, // 100ms max for network operations
        }
    }
}

/// Enhanced component wrapper preserving existing interfaces
pub struct Enhanced<T, E> {
    /// Base implementation (existing)
    pub base: T,
    /// ML enhancement (optional)
    pub enhancement: Option<Arc<E>>,
    /// Configuration
    pub config: MLConfig,
}

impl<T, E> Enhanced<T, E> {
    /// Create new enhanced component with ML capability
    pub fn new(base: T, enhancement: Option<E>, config: MLConfig) -> Self {
        Self {
            base,
            enhancement: enhancement.map(Arc::new),
            config,
        }
    }

    /// Create enhanced component without ML (fallback mode)
    pub fn fallback_only(base: T) -> Self {
        Self {
            base,
            enhancement: None,
            config: MLConfig {
                enabled: false,
                ..Default::default()
            },
        }
    }

    /// Check if ML enhancement is available and enabled
    pub fn has_enhancement(&self) -> bool {
        self.config.enabled && self.enhancement.is_some()
    }
}

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
/// Trait for ML backends (Candle, ONNX, etc.)
pub trait MLBackend: Send + Sync {
    type Input;
    type Output;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Run inference with timeout protection
    async fn predict(&self, input: Self::Input) -> Result<Self::Output, Self::Error>;

    /// Get backend information for telemetry
    fn backend_info(&self) -> &'static str;
}

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
/// Feature vector for ML models
#[derive(Clone, Debug)]
pub struct FeatureVector {
    /// Features as a flat array
    pub features: Vec<f32>,
    /// Feature names for debugging
    pub names: Vec<String>,
    /// Timestamp when features were extracted
    pub timestamp: std::time::Instant,
}

impl FeatureVector {
    /// Create new feature vector
    pub fn new(features: Vec<f32>, names: Vec<String>) -> Self {
        assert_eq!(features.len(), names.len(), "Feature count must match name count");
        Self {
            features,
            names,
            timestamp: std::time::Instant::now(),
        }
    }

    /// Get feature dimension
    pub fn dim(&self) -> usize {
        self.features.len()
    }

    /// Check if features are stale (older than threshold)
    pub fn is_stale(&self, max_age: std::time::Duration) -> bool {
        self.timestamp.elapsed() > max_age
    }
}

#[cfg(any(feature = "ml-routing", feature = "rl-congestion"))]
/// ML prediction result
#[derive(Clone, Debug)]
pub struct Prediction {
    /// Predicted value(s)
    pub values: Vec<f32>,
    /// Confidence score (0.0 to 1.0)
    pub confidence: f32,
    /// Model version used for prediction
    pub model_version: String,
    /// Inference latency
    pub latency: std::time::Duration,
}

impl Prediction {
    /// Create new prediction
    pub fn new(values: Vec<f32>, confidence: f32, model_version: String, latency: std::time::Duration) -> Self {
        Self {
            values,
            confidence,
            model_version,
            latency,
        }
    }

    /// Get primary prediction value
    pub fn primary_value(&self) -> f32 {
        self.values.first().copied().unwrap_or(0.0)
    }

    /// Check if prediction is high confidence
    pub fn is_high_confidence(&self, threshold: f32) -> bool {
        self.confidence >= threshold
    }
}