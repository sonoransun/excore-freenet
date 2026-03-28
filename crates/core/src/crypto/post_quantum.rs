//! Quantum-Safe Hybrid Cryptography
//!
//! This module implements hybrid post-quantum cryptography combining classical
//! algorithms with quantum-resistant ones for future-proof security.
//!
//! ## Features
//!
//! - **Hybrid Key Exchange**: ML-KEM + X25519 for backward compatibility
//! - **Hybrid Signatures**: Dilithium + Ed25519 for dual protection
//! - **Protocol Negotiation**: Automatic capability detection
//! - **Graceful Fallback**: Classical crypto when quantum-safe unavailable

use crate::ml::{Enhanced, MLConfig};
use crate::transport::{TransportKeypair, TransportPublicKey};

use anyhow::{Result, Context, bail};
use std::sync::Arc;
use std::time::{Duration, Instant};
use bytes::{Bytes, BytesMut};
use serde::{Serialize, Deserialize};

#[cfg(feature = "quantum-safe")]
use ml_kem::{MlKem768, KemSharedSecret};
#[cfg(feature = "quantum-safe")]
use dilithium::{Dilithium3, Signature as DilithiumSignature, VerifyingKey as DilithiumVerifyingKey};

/// Post-quantum enhanced transport keypair
pub type PostQuantumTransport = Enhanced<TransportKeypair, PostQuantumEnhancer>;

impl PostQuantumTransport {
    /// Create new post-quantum enhanced transport
    pub fn new(base: TransportKeypair, config: PostQuantumConfig) -> Result<Self> {
        let enhancement = if config.enabled {
            Some(PostQuantumEnhancer::new(config.clone())?)
        } else {
            None
        };

        let ml_config = MLConfig {
            enabled: config.enabled,
            fallback_on_error: config.fallback_on_error,
            model_path: None,
            max_inference_latency_ms: 50, // 50ms max for crypto operations
        };

        Ok(Enhanced::new(base, enhancement, ml_config))
    }

    /// Generate hybrid key exchange with quantum-safe ML-KEM
    pub fn hybrid_key_exchange(&self, peer_public: &PostQuantumPublicKey) -> Result<HybridSharedSecret> {
        if let Some(ref enhancer) = self.enhancement {
            enhancer.hybrid_key_exchange(&self.base, peer_public)
        } else {
            // Fallback to classical X25519
            let classical_secret = self.base.diffie_hellman(&peer_public.classical)?;
            Ok(HybridSharedSecret {
                classical: classical_secret,
                quantum_safe: None,
                protocol_version: ProtocolVersion::Classical,
            })
        }
    }

    /// Create hybrid signature with both classical and post-quantum
    pub fn hybrid_sign(&self, message: &[u8]) -> Result<HybridSignature> {
        if let Some(ref enhancer) = self.enhancement {
            enhancer.hybrid_sign(&self.base, message)
        } else {
            // Fallback to classical Ed25519
            let classical_signature = self.base.sign(message)?;
            Ok(HybridSignature {
                classical: classical_signature,
                quantum_safe: None,
                protocol_version: ProtocolVersion::Classical,
            })
        }
    }

    /// Get hybrid public key for this transport
    pub fn hybrid_public_key(&self) -> Result<PostQuantumPublicKey> {
        if let Some(ref enhancer) = self.enhancement {
            enhancer.get_public_key(&self.base)
        } else {
            Ok(PostQuantumPublicKey {
                classical: self.base.public_key(),
                quantum_safe: None,
                protocol_version: ProtocolVersion::Classical,
            })
        }
    }
}

/// Post-quantum cryptography enhancer
pub struct PostQuantumEnhancer {
    #[cfg(feature = "quantum-safe")]
    ml_kem_keypair: Arc<MlKem768>,
    #[cfg(feature = "quantum-safe")]
    dilithium_keypair: Arc<Dilithium3>,
    config: PostQuantumConfig,
    stats: Arc<PostQuantumStats>,
}

impl PostQuantumEnhancer {
    /// Create new post-quantum enhancer
    pub fn new(config: PostQuantumConfig) -> Result<Self> {
        let stats = Arc::new(PostQuantumStats::default());

        #[cfg(feature = "quantum-safe")]
        {
            let ml_kem_keypair = Arc::new(
                MlKem768::keygen(&mut rand::thread_rng())
                    .context("Failed to generate ML-KEM keypair")?
            );

            let dilithium_keypair = Arc::new(
                Dilithium3::keygen(&mut rand::thread_rng())
                    .context("Failed to generate Dilithium keypair")?
            );

            Ok(Self {
                ml_kem_keypair,
                dilithium_keypair,
                config,
                stats,
            })
        }

        #[cfg(not(feature = "quantum-safe"))]
        {
            Ok(Self {
                config,
                stats,
            })
        }
    }

    /// Perform hybrid key exchange
    pub fn hybrid_key_exchange(
        &self,
        base: &TransportKeypair,
        peer_public: &PostQuantumPublicKey,
    ) -> Result<HybridSharedSecret> {
        let start = Instant::now();

        // Always perform classical key exchange
        let classical_secret = base.diffie_hellman(&peer_public.classical)?;

        #[cfg(feature = "quantum-safe")]
        let quantum_safe_secret = if let Some(peer_ml_kem) = &peer_public.quantum_safe {
            // Perform ML-KEM encapsulation
            let (ciphertext, shared_secret) = self.ml_kem_keypair
                .encaps(&peer_ml_kem.ml_kem_public, &mut rand::thread_rng())
                .context("ML-KEM encapsulation failed")?;

            Some(QuantumSafeSecret {
                shared_secret,
                ciphertext,
            })
        } else {
            None
        };

        #[cfg(not(feature = "quantum-safe"))]
        let quantum_safe_secret = None;

        let protocol_version = if quantum_safe_secret.is_some() {
            ProtocolVersion::Hybrid
        } else {
            ProtocolVersion::Classical
        };

        // Update statistics
        let elapsed = start.elapsed();
        self.stats.key_exchanges.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.stats.key_exchange_time_ns.fetch_add(elapsed.as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);

        Ok(HybridSharedSecret {
            classical: classical_secret,
            quantum_safe: quantum_safe_secret,
            protocol_version,
        })
    }

    /// Create hybrid signature
    pub fn hybrid_sign(&self, base: &TransportKeypair, message: &[u8]) -> Result<HybridSignature> {
        let start = Instant::now();

        // Always create classical signature
        let classical_signature = base.sign(message)?;

        #[cfg(feature = "quantum-safe")]
        let quantum_safe_signature = if self.config.enable_hybrid_signatures {
            let dilithium_sig = self.dilithium_keypair
                .sign(message, &mut rand::thread_rng())
                .context("Dilithium signature failed")?;
            Some(dilithium_sig)
        } else {
            None
        };

        #[cfg(not(feature = "quantum-safe"))]
        let quantum_safe_signature = None;

        let protocol_version = if quantum_safe_signature.is_some() {
            ProtocolVersion::Hybrid
        } else {
            ProtocolVersion::Classical
        };

        // Update statistics
        let elapsed = start.elapsed();
        self.stats.signatures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.stats.signature_time_ns.fetch_add(elapsed.as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);

        Ok(HybridSignature {
            classical: classical_signature,
            quantum_safe: quantum_safe_signature,
            protocol_version,
        })
    }

    /// Get hybrid public key
    pub fn get_public_key(&self, base: &TransportKeypair) -> Result<PostQuantumPublicKey> {
        let classical = base.public_key();

        #[cfg(feature = "quantum-safe")]
        let quantum_safe = if self.config.enable_quantum_safe {
            Some(QuantumSafePublicKey {
                ml_kem_public: self.ml_kem_keypair.public_key(),
                dilithium_public: self.dilithium_keypair.verifying_key(),
            })
        } else {
            None
        };

        #[cfg(not(feature = "quantum-safe"))]
        let quantum_safe = None;

        let protocol_version = if quantum_safe.is_some() {
            ProtocolVersion::Hybrid
        } else {
            ProtocolVersion::Classical
        };

        Ok(PostQuantumPublicKey {
            classical,
            quantum_safe,
            protocol_version,
        })
    }

    /// Get performance statistics
    pub fn get_stats(&self) -> PostQuantumStats {
        (*self.stats).clone()
    }
}

/// Post-quantum configuration
#[derive(Clone, Debug)]
pub struct PostQuantumConfig {
    /// Enable post-quantum cryptography
    pub enabled: bool,
    /// Enable quantum-safe key exchange (ML-KEM)
    pub enable_quantum_safe: bool,
    /// Enable hybrid signatures (Dilithium + Ed25519)
    pub enable_hybrid_signatures: bool,
    /// Fall back to classical crypto on error
    pub fallback_on_error: bool,
    /// Maximum latency for post-quantum operations
    pub max_latency: Duration,
}

impl Default for PostQuantumConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enable_quantum_safe: true,
            enable_hybrid_signatures: true,
            fallback_on_error: true,
            max_latency: Duration::from_millis(100),
        }
    }
}

/// Protocol version negotiation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolVersion {
    /// Classical cryptography only
    Classical,
    /// Hybrid classical + post-quantum
    Hybrid,
    /// Post-quantum only (future)
    QuantumOnly,
}

/// Hybrid public key containing both classical and post-quantum components
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostQuantumPublicKey {
    /// Classical X25519 public key
    pub classical: TransportPublicKey,
    /// Post-quantum components (optional for backward compatibility)
    pub quantum_safe: Option<QuantumSafePublicKey>,
    /// Protocol version
    pub protocol_version: ProtocolVersion,
}

/// Quantum-safe public key components
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantumSafePublicKey {
    #[cfg(feature = "quantum-safe")]
    /// ML-KEM public key for key exchange
    pub ml_kem_public: ml_kem::PublicKey,
    #[cfg(feature = "quantum-safe")]
    /// Dilithium verifying key for signatures
    pub dilithium_public: DilithiumVerifyingKey,
}

/// Hybrid shared secret from key exchange
#[derive(Debug)]
pub struct HybridSharedSecret {
    /// Classical X25519 shared secret
    pub classical: [u8; 32],
    /// Post-quantum shared secret (optional)
    pub quantum_safe: Option<QuantumSafeSecret>,
    /// Protocol version used
    pub protocol_version: ProtocolVersion,
}

impl HybridSharedSecret {
    /// Derive final shared key using HKDF
    pub fn derive_key(&self, info: &[u8]) -> Result<[u8; 32]> {
        use hkdf::Hkdf;
        use sha2::Sha256;

        match &self.quantum_safe {
            Some(qs_secret) => {
                // Combine classical and quantum-safe secrets
                let mut combined = BytesMut::with_capacity(32 + qs_secret.shared_secret.len());
                combined.extend_from_slice(&self.classical);
                combined.extend_from_slice(&qs_secret.shared_secret);

                let hk = Hkdf::<Sha256>::new(None, &combined);
                let mut key = [0u8; 32];
                hk.expand(info, &mut key)
                    .context("HKDF expansion failed")?;
                Ok(key)
            }
            None => {
                // Classical only
                let hk = Hkdf::<Sha256>::new(None, &self.classical);
                let mut key = [0u8; 32];
                hk.expand(info, &mut key)
                    .context("HKDF expansion failed")?;
                Ok(key)
            }
        }
    }
}

/// Quantum-safe secret from ML-KEM
#[derive(Debug)]
pub struct QuantumSafeSecret {
    #[cfg(feature = "quantum-safe")]
    /// Shared secret from ML-KEM
    pub shared_secret: KemSharedSecret,
    #[cfg(feature = "quantum-safe")]
    /// Ciphertext for decapsulation
    pub ciphertext: ml_kem::Ciphertext,
}

/// Hybrid signature containing both classical and post-quantum signatures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSignature {
    /// Classical Ed25519 signature
    pub classical: Vec<u8>,
    /// Post-quantum Dilithium signature (optional)
    pub quantum_safe: Option<Vec<u8>>,
    /// Protocol version used
    pub protocol_version: ProtocolVersion,
}

impl HybridSignature {
    /// Verify hybrid signature
    pub fn verify(&self, message: &[u8], public_key: &PostQuantumPublicKey) -> Result<bool> {
        // Always verify classical signature
        let classical_valid = public_key.classical.verify(message, &self.classical)?;

        match (&self.quantum_safe, &public_key.quantum_safe) {
            (Some(qs_sig), Some(qs_pub)) => {
                #[cfg(feature = "quantum-safe")]
                {
                    let dilithium_sig = DilithiumSignature::from_bytes(qs_sig)
                        .map_err(|e| anyhow::anyhow!("Invalid Dilithium signature: {}", e))?;

                    let dilithium_valid = qs_pub.dilithium_public
                        .verify(message, &dilithium_sig)
                        .is_ok();

                    // Both signatures must be valid for hybrid verification
                    Ok(classical_valid && dilithium_valid)
                }

                #[cfg(not(feature = "quantum-safe"))]
                Ok(classical_valid)
            }
            _ => {
                // Classical only verification
                Ok(classical_valid)
            }
        }
    }
}

/// Post-quantum cryptography statistics
#[derive(Debug, Clone, Default)]
pub struct PostQuantumStats {
    /// Number of hybrid key exchanges performed
    pub key_exchanges: std::sync::atomic::AtomicU64,
    /// Time spent on key exchanges (nanoseconds)
    pub key_exchange_time_ns: std::sync::atomic::AtomicU64,
    /// Number of hybrid signatures created
    pub signatures: std::sync::atomic::AtomicU64,
    /// Time spent on signatures (nanoseconds)
    pub signature_time_ns: std::sync::atomic::AtomicU64,
    /// Number of fallbacks to classical crypto
    pub fallbacks: std::sync::atomic::AtomicU64,
}

impl PostQuantumStats {
    /// Calculate average key exchange latency
    pub fn avg_key_exchange_latency_ns(&self) -> f64 {
        let total_time = self.key_exchange_time_ns.load(std::sync::atomic::Ordering::Relaxed) as f64;
        let total_ops = self.key_exchanges.load(std::sync::atomic::Ordering::Relaxed) as f64;
        if total_ops > 0.0 { total_time / total_ops } else { 0.0 }
    }

    /// Calculate average signature latency
    pub fn avg_signature_latency_ns(&self) -> f64 {
        let total_time = self.signature_time_ns.load(std::sync::atomic::Ordering::Relaxed) as f64;
        let total_ops = self.signatures.load(std::sync::atomic::Ordering::Relaxed) as f64;
        if total_ops > 0.0 { total_time / total_ops } else { 0.0 }
    }

    /// Calculate quantum-safe utilization rate
    pub fn quantum_safe_utilization(&self) -> f64 {
        let total_ops = self.key_exchanges.load(std::sync::atomic::Ordering::Relaxed) as f64;
        let fallbacks = self.fallbacks.load(std::sync::atomic::Ordering::Relaxed) as f64;
        if total_ops > 0.0 { (total_ops - fallbacks) / total_ops } else { 0.0 }
    }
}

/// Protocol negotiation utilities
pub struct ProtocolNegotiation;

impl ProtocolNegotiation {
    /// Negotiate best protocol version between peers
    pub fn negotiate(local_caps: &[ProtocolVersion], remote_caps: &[ProtocolVersion]) -> ProtocolVersion {
        // Prefer hybrid if both support it
        if local_caps.contains(&ProtocolVersion::Hybrid) && remote_caps.contains(&ProtocolVersion::Hybrid) {
            return ProtocolVersion::Hybrid;
        }

        // Future: prefer quantum-only if both support it
        if local_caps.contains(&ProtocolVersion::QuantumOnly) && remote_caps.contains(&ProtocolVersion::QuantumOnly) {
            return ProtocolVersion::QuantumOnly;
        }

        // Fallback to classical
        ProtocolVersion::Classical
    }

    /// Get supported protocol versions based on feature flags
    pub fn supported_versions() -> Vec<ProtocolVersion> {
        let mut versions = vec![ProtocolVersion::Classical];

        #[cfg(feature = "quantum-safe")]
        versions.push(ProtocolVersion::Hybrid);

        versions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_negotiation() {
        let local = vec![ProtocolVersion::Classical, ProtocolVersion::Hybrid];
        let remote = vec![ProtocolVersion::Classical, ProtocolVersion::Hybrid];

        let negotiated = ProtocolNegotiation::negotiate(&local, &remote);
        assert_eq!(negotiated, ProtocolVersion::Hybrid);
    }

    #[test]
    fn test_fallback_negotiation() {
        let local = vec![ProtocolVersion::Classical, ProtocolVersion::Hybrid];
        let remote = vec![ProtocolVersion::Classical];

        let negotiated = ProtocolNegotiation::negotiate(&local, &remote);
        assert_eq!(negotiated, ProtocolVersion::Classical);
    }

    #[cfg(feature = "quantum-safe")]
    #[test]
    fn test_post_quantum_config_default() {
        let config = PostQuantumConfig::default();
        assert!(config.enabled);
        assert!(config.enable_quantum_safe);
        assert!(config.enable_hybrid_signatures);
        assert!(config.fallback_on_error);
    }

    #[test]
    fn test_stats_calculations() {
        let stats = PostQuantumStats::default();
        stats.key_exchanges.store(10, std::sync::atomic::Ordering::Relaxed);
        stats.key_exchange_time_ns.store(1_000_000, std::sync::atomic::Ordering::Relaxed);

        let avg_latency = stats.avg_key_exchange_latency_ns();
        assert_eq!(avg_latency, 100_000.0); // 100μs average
    }
}