//! Cryptographic Infrastructure for Freenet Core
//!
//! This module provides cryptographic capabilities including:
//! - Classical cryptography (X25519, Ed25519, ChaCha20Poly1305)
//! - Post-quantum cryptography (ML-KEM, Dilithium)
//! - Hybrid schemes for backward compatibility and future-proofing

#[cfg(feature = "quantum-safe")]
pub mod post_quantum;

/// Fully homomorphic encryption for privacy-preserving contract execution
#[cfg(feature = "homomorphic-contracts")]
pub mod fhe;

// Re-export key types for easier access
#[cfg(feature = "quantum-safe")]
pub use post_quantum::{
    PostQuantumTransport, PostQuantumConfig, PostQuantumPublicKey,
    HybridSharedSecret, HybridSignature, ProtocolVersion, ProtocolNegotiation
};

#[cfg(feature = "homomorphic-contracts")]
pub use fhe::{
    FHEContractExecutor, FHEEnhancer, FHEConfig, FHEParameterSet, FHEStats,
    EncryptedState, EncryptedOperation, EncryptedExecutionResult,
    EncryptionMetadata, ExecutionMetadata, ComputationStats
};

use anyhow::Result;

/// Enhanced cryptography manager for coordinating different crypto backends
pub struct CryptoManager {
    #[cfg(feature = "quantum-safe")]
    post_quantum_config: Option<post_quantum::PostQuantumConfig>,
    #[cfg(feature = "homomorphic-contracts")]
    fhe_config: Option<fhe::FHEConfig>,
}

impl CryptoManager {
    /// Create new crypto manager
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "quantum-safe")]
            post_quantum_config: Some(post_quantum::PostQuantumConfig::default()),
            #[cfg(feature = "homomorphic-contracts")]
            fhe_config: Some(fhe::FHEConfig::default()),
        }
    }

    /// Create crypto manager with custom post-quantum config
    #[cfg(feature = "quantum-safe")]
    pub fn with_post_quantum_config(config: post_quantum::PostQuantumConfig) -> Self {
        Self {
            post_quantum_config: Some(config),
            #[cfg(feature = "homomorphic-contracts")]
            fhe_config: Some(fhe::FHEConfig::default()),
        }
    }

    /// Create crypto manager with custom FHE config
    #[cfg(feature = "homomorphic-contracts")]
    pub fn with_fhe_config(config: fhe::FHEConfig) -> Self {
        Self {
            #[cfg(feature = "quantum-safe")]
            post_quantum_config: Some(post_quantum::PostQuantumConfig::default()),
            fhe_config: Some(config),
        }
    }

    /// Create crypto manager without post-quantum features
    pub fn classical_only() -> Self {
        Self {
            #[cfg(feature = "quantum-safe")]
            post_quantum_config: None,
            #[cfg(feature = "homomorphic-contracts")]
            fhe_config: None,
        }
    }

    /// Check if post-quantum cryptography is available
    pub fn has_post_quantum(&self) -> bool {
        #[cfg(feature = "quantum-safe")]
        {
            self.post_quantum_config.is_some()
        }
        #[cfg(not(feature = "quantum-safe"))]
        {
            false
        }
    }

    /// Check if FHE capabilities are available
    pub fn has_fhe(&self) -> bool {
        #[cfg(feature = "homomorphic-contracts")]
        {
            self.fhe_config.is_some()
        }
        #[cfg(not(feature = "homomorphic-contracts"))]
        {
            false
        }
    }

    /// Get supported protocol versions
    pub fn supported_protocols(&self) -> Vec<String> {
        let mut protocols = vec!["Classical".to_string()];

        #[cfg(feature = "quantum-safe")]
        if self.post_quantum_config.is_some() {
            protocols.extend(vec!["PostQuantum".to_string(), "Hybrid".to_string()]);
        }

        #[cfg(feature = "homomorphic-contracts")]
        if self.fhe_config.is_some() {
            protocols.push("FHE".to_string());
        }

        protocols
    }

    /// Get FHE configuration
    #[cfg(feature = "homomorphic-contracts")]
    pub fn fhe_config(&self) -> Option<&fhe::FHEConfig> {
        self.fhe_config.as_ref()
    }

    /// Get post-quantum configuration
    #[cfg(feature = "quantum-safe")]
    pub fn post_quantum_config(&self) -> Option<&post_quantum::PostQuantumConfig> {
        self.post_quantum_config.as_ref()
    }
}

impl Default for CryptoManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crypto_manager_creation() {
        let manager = CryptoManager::new();

        #[cfg(feature = "quantum-safe")]
        assert!(manager.has_post_quantum());

        #[cfg(not(feature = "quantum-safe"))]
        assert!(!manager.has_post_quantum());
    }

    #[test]
    fn test_classical_only_manager() {
        let manager = CryptoManager::classical_only();
        assert!(!manager.has_post_quantum());
        assert!(!manager.has_fhe());

        let protocols = manager.supported_protocols();
        assert!(protocols.contains(&"Classical".to_string()));
    }

    #[cfg(feature = "quantum-safe")]
    #[test]
    fn test_post_quantum_config() {
        let config = post_quantum::PostQuantumConfig {
            enabled: true,
            enable_quantum_safe: true,
            enable_hybrid_signatures: false, // Custom config
            ..Default::default()
        };

        let manager = CryptoManager::with_post_quantum_config(config);
        assert!(manager.has_post_quantum());
    }

    #[cfg(feature = "homomorphic-contracts")]
    #[test]
    fn test_fhe_config() {
        let config = fhe::FHEConfig {
            enabled: true,
            fallback_on_error: false, // Custom config
            parameter_set: fhe::FHEParameterSet::Secure,
            ..Default::default()
        };

        let manager = CryptoManager::with_fhe_config(config);
        assert!(manager.has_fhe());
        assert!(manager.fhe_config().is_some());
        assert_eq!(manager.fhe_config().unwrap().parameter_set, fhe::FHEParameterSet::Secure);
    }

    #[test]
    fn test_supported_protocols() {
        let manager = CryptoManager::new();
        let protocols = manager.supported_protocols();

        assert!(protocols.contains(&"Classical".to_string()));

        #[cfg(feature = "quantum-safe")]
        {
            assert!(protocols.contains(&"PostQuantum".to_string()));
            assert!(protocols.contains(&"Hybrid".to_string()));
        }

        #[cfg(feature = "homomorphic-contracts")]
        {
            assert!(protocols.contains(&"FHE".to_string()));
        }
    }
}