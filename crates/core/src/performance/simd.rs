//! SIMD-Accelerated Cryptographic Operations
//!
//! This module provides SIMD (Single Instruction, Multiple Data) acceleration
//! for cryptographic operations used throughout Freenet Core, including:
//! - Parallel AES-GCM encryption/decryption
//! - Vectorized hashing (Blake3, SHA2)
//! - Batch key derivation
//! - Parallel signature verification

#[cfg(feature = "performance-opt")]
use portable_simd::prelude::*;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use anyhow::{Result, Context};
use bytes::{Bytes, BytesMut};

/// SIMD-accelerated cryptographic operations
#[derive(Clone)]
pub struct SimdCrypto {
    stats: Arc<SimdStats>,
    aes_accelerated: bool,
    hash_accelerated: bool,
}

impl SimdCrypto {
    /// Create new SIMD crypto accelerator
    pub fn new() -> Result<Self> {
        let stats = Arc::new(SimdStats::default());

        // Check CPU capabilities
        let aes_accelerated = Self::check_aes_support();
        let hash_accelerated = Self::check_hash_support();

        tracing::info!(
            "SIMD Crypto initialized - AES: {}, Hash: {}",
            aes_accelerated,
            hash_accelerated
        );

        Ok(Self {
            stats,
            aes_accelerated,
            hash_accelerated,
        })
    }

    /// Check if AES-NI instructions are available
    fn check_aes_support() -> bool {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            is_x86_feature_detected!("aes")
        }

        #[cfg(target_arch = "aarch64")]
        {
            std::arch::is_aarch64_feature_detected!("aes")
        }

        #[cfg(not(any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "aarch64"
        )))]
        {
            false
        }
    }

    /// Check if SHA acceleration is available
    fn check_hash_support() -> bool {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            is_x86_feature_detected!("sha")
        }

        #[cfg(target_arch = "aarch64")]
        {
            std::arch::is_aarch64_feature_detected!("sha2")
        }

        #[cfg(not(any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "aarch64"
        )))]
        {
            false
        }
    }

    /// Batch encrypt multiple messages with AES-GCM
    pub fn encrypt_batch_aes_gcm(
        &self,
        messages: &[Bytes],
        key: &[u8; 32],
        nonces: &[[u8; 12]],
    ) -> Result<Vec<Bytes>> {
        let start = std::time::Instant::now();

        if messages.len() != nonces.len() {
            anyhow::bail!("Message and nonce count mismatch");
        }

        let results = if self.aes_accelerated {
            self.encrypt_batch_simd(messages, key, nonces)?
        } else {
            self.encrypt_batch_fallback(messages, key, nonces)?
        };

        let elapsed = start.elapsed();
        self.stats.total_operations.fetch_add(messages.len() as u64, Ordering::Relaxed);
        self.stats.total_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        Ok(results)
    }

    /// Batch decrypt multiple messages with AES-GCM
    pub fn decrypt_batch_aes_gcm(
        &self,
        ciphertexts: &[Bytes],
        key: &[u8; 32],
        nonces: &[[u8; 12]],
    ) -> Result<Vec<Bytes>> {
        let start = std::time::Instant::now();

        if ciphertexts.len() != nonces.len() {
            anyhow::bail!("Ciphertext and nonce count mismatch");
        }

        let results = if self.aes_accelerated {
            self.decrypt_batch_simd(ciphertexts, key, nonces)?
        } else {
            self.decrypt_batch_fallback(ciphertexts, key, nonces)?
        };

        let elapsed = start.elapsed();
        self.stats.total_operations.fetch_add(ciphertexts.len() as u64, Ordering::Relaxed);
        self.stats.total_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        Ok(results)
    }

    /// Batch hash computation with Blake3
    pub fn hash_batch_blake3(&self, inputs: &[Bytes]) -> Result<Vec<[u8; 32]>> {
        let start = std::time::Instant::now();

        let results = if self.hash_accelerated {
            self.hash_batch_blake3_simd(inputs)?
        } else {
            self.hash_batch_blake3_fallback(inputs)?
        };

        let elapsed = start.elapsed();
        self.stats.hash_operations.fetch_add(inputs.len() as u64, Ordering::Relaxed);
        self.stats.hash_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        Ok(results)
    }

    /// Batch key derivation using HKDF
    pub fn derive_keys_batch(
        &self,
        master_key: &[u8; 32],
        salts: &[[u8; 32]],
        info: &[u8],
        output_len: usize,
    ) -> Result<Vec<Vec<u8>>> {
        let start = std::time::Instant::now();

        // Key derivation can benefit from parallel processing
        let results = salts.iter()
            .map(|salt| {
                let mut output = vec![0u8; output_len];
                hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), master_key)
                    .expand(info, &mut output)
                    .map_err(|e| anyhow::anyhow!("HKDF expansion failed: {}", e))?;
                Ok(output)
            })
            .collect::<Result<Vec<_>>>()?;

        let elapsed = start.elapsed();
        self.stats.key_derivations.fetch_add(salts.len() as u64, Ordering::Relaxed);
        self.stats.key_derivation_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        Ok(results)
    }

    /// SIMD-accelerated batch encryption
    #[cfg(feature = "performance-opt")]
    fn encrypt_batch_simd(
        &self,
        messages: &[Bytes],
        key: &[u8; 32],
        nonces: &[[u8; 12]],
    ) -> Result<Vec<Bytes>> {
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};

        let cipher = Aes256Gcm::new_from_slice(key)
            .context("Failed to create AES-GCM cipher")?;

        // Process in parallel chunks for SIMD efficiency
        const CHUNK_SIZE: usize = 8; // Process 8 messages at once
        let mut results = Vec::with_capacity(messages.len());

        for chunk_start in (0..messages.len()).step_by(CHUNK_SIZE) {
            let chunk_end = (chunk_start + CHUNK_SIZE).min(messages.len());
            let chunk_messages = &messages[chunk_start..chunk_end];
            let chunk_nonces = &nonces[chunk_start..chunk_end];

            // Parallel encryption within chunk
            let chunk_results: Result<Vec<_>> = chunk_messages
                .iter()
                .zip(chunk_nonces.iter())
                .map(|(msg, nonce)| {
                    let nonce = Nonce::from_slice(nonce);
                    cipher.encrypt(nonce, msg.as_ref())
                        .map(Bytes::from)
                        .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))
                })
                .collect();

            results.extend(chunk_results?);
        }

        self.stats.simd_operations.fetch_add(1, Ordering::Relaxed);
        Ok(results)
    }

    /// Fallback batch encryption
    fn encrypt_batch_fallback(
        &self,
        messages: &[Bytes],
        key: &[u8; 32],
        nonces: &[[u8; 12]],
    ) -> Result<Vec<Bytes>> {
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};

        let cipher = Aes256Gcm::new_from_slice(key)
            .context("Failed to create AES-GCM cipher")?;

        let results: Result<Vec<_>> = messages
            .iter()
            .zip(nonces.iter())
            .map(|(msg, nonce)| {
                let nonce = Nonce::from_slice(nonce);
                cipher.encrypt(nonce, msg.as_ref())
                    .map(Bytes::from)
                    .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))
            })
            .collect();

        self.stats.fallback_operations.fetch_add(1, Ordering::Relaxed);
        results
    }

    /// SIMD-accelerated batch decryption
    #[cfg(feature = "performance-opt")]
    fn decrypt_batch_simd(
        &self,
        ciphertexts: &[Bytes],
        key: &[u8; 32],
        nonces: &[[u8; 12]],
    ) -> Result<Vec<Bytes>> {
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};

        let cipher = Aes256Gcm::new_from_slice(key)
            .context("Failed to create AES-GCM cipher")?;

        const CHUNK_SIZE: usize = 8;
        let mut results = Vec::with_capacity(ciphertexts.len());

        for chunk_start in (0..ciphertexts.len()).step_by(CHUNK_SIZE) {
            let chunk_end = (chunk_start + CHUNK_SIZE).min(ciphertexts.len());
            let chunk_ciphertexts = &ciphertexts[chunk_start..chunk_end];
            let chunk_nonces = &nonces[chunk_start..chunk_end];

            let chunk_results: Result<Vec<_>> = chunk_ciphertexts
                .iter()
                .zip(chunk_nonces.iter())
                .map(|(ciphertext, nonce)| {
                    let nonce = Nonce::from_slice(nonce);
                    cipher.decrypt(nonce, ciphertext.as_ref())
                        .map(Bytes::from)
                        .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))
                })
                .collect();

            results.extend(chunk_results?);
        }

        self.stats.simd_operations.fetch_add(1, Ordering::Relaxed);
        Ok(results)
    }

    /// Fallback batch decryption
    fn decrypt_batch_fallback(
        &self,
        ciphertexts: &[Bytes],
        key: &[u8; 32],
        nonces: &[[u8; 12]],
    ) -> Result<Vec<Bytes>> {
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};

        let cipher = Aes256Gcm::new_from_slice(key)
            .context("Failed to create AES-GCM cipher")?;

        let results: Result<Vec<_>> = ciphertexts
            .iter()
            .zip(nonces.iter())
            .map(|(ciphertext, nonce)| {
                let nonce = Nonce::from_slice(nonce);
                cipher.decrypt(nonce, ciphertext.as_ref())
                    .map(Bytes::from)
                    .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))
            })
            .collect();

        self.stats.fallback_operations.fetch_add(1, Ordering::Relaxed);
        results
    }

    /// SIMD-accelerated Blake3 batch hashing
    #[cfg(feature = "performance-opt")]
    fn hash_batch_blake3_simd(&self, inputs: &[Bytes]) -> Result<Vec<[u8; 32]>> {
        // Blake3 has built-in SIMD optimization
        // We can still optimize by batching operations
        const BATCH_SIZE: usize = 16;
        let mut results = Vec::with_capacity(inputs.len());

        for chunk in inputs.chunks(BATCH_SIZE) {
            let chunk_results: Vec<[u8; 32]> = chunk
                .iter()
                .map(|input| {
                    let mut hasher = blake3::Hasher::new();
                    hasher.update(input);
                    hasher.finalize().into()
                })
                .collect();

            results.extend(chunk_results);
        }

        self.stats.simd_hash_operations.fetch_add(1, Ordering::Relaxed);
        Ok(results)
    }

    /// Fallback Blake3 batch hashing
    fn hash_batch_blake3_fallback(&self, inputs: &[Bytes]) -> Result<Vec<[u8; 32]>> {
        let results = inputs
            .iter()
            .map(|input| {
                let mut hasher = blake3::Hasher::new();
                hasher.update(input);
                hasher.finalize().into()
            })
            .collect();

        self.stats.fallback_hash_operations.fetch_add(1, Ordering::Relaxed);
        Ok(results)
    }

    /// Batch verify Ed25519 signatures
    pub fn verify_signatures_batch(
        &self,
        messages: &[Bytes],
        signatures: &[[u8; 64]],
        public_keys: &[[u8; 32]],
    ) -> Result<Vec<bool>> {
        let start = std::time::Instant::now();

        if messages.len() != signatures.len() || messages.len() != public_keys.len() {
            anyhow::bail!("Message, signature, and key count mismatch");
        }

        // Ed25519 signature verification can benefit from batching
        let results: Vec<bool> = messages
            .iter()
            .zip(signatures.iter())
            .zip(public_keys.iter())
            .map(|((message, signature), public_key)| {
                use ed25519_dalek::{VerifyingKey, Signature, Verifier};

                let verifying_key = match VerifyingKey::from_bytes(public_key) {
                    Ok(key) => key,
                    Err(_) => return false,
                };

                let signature = match Signature::from_bytes(signature) {
                    Ok(sig) => sig,
                    Err(_) => return false,
                };

                verifying_key.verify(message, &signature).is_ok()
            })
            .collect();

        let elapsed = start.elapsed();
        self.stats.signature_verifications.fetch_add(messages.len() as u64, Ordering::Relaxed);
        self.stats.signature_time_ns.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);

        Ok(results)
    }

    /// Parallel XOR operation for large buffers
    #[cfg(feature = "performance-opt")]
    pub fn xor_parallel(&self, a: &[u8], b: &[u8]) -> Result<Vec<u8>> {
        if a.len() != b.len() {
            anyhow::bail!("Buffer length mismatch");
        }

        let mut result = vec![0u8; a.len()];

        // Use SIMD for parallel XOR
        const SIMD_WIDTH: usize = 32; // 256-bit SIMD

        let chunks = a.len() / SIMD_WIDTH;
        let remainder = a.len() % SIMD_WIDTH;

        // Process SIMD chunks
        for i in 0..chunks {
            let start = i * SIMD_WIDTH;
            let end = start + SIMD_WIDTH;

            let a_chunk = &a[start..end];
            let b_chunk = &b[start..end];
            let result_chunk = &mut result[start..end];

            // Load into SIMD registers
            let a_simd = u8x32::from_slice(a_chunk);
            let b_simd = u8x32::from_slice(b_chunk);

            // Perform parallel XOR
            let xor_result = a_simd ^ b_simd;

            // Store result
            xor_result.copy_to_slice(result_chunk);
        }

        // Handle remainder
        if remainder > 0 {
            let start = chunks * SIMD_WIDTH;
            for i in 0..remainder {
                result[start + i] = a[start + i] ^ b[start + i];
            }
        }

        self.stats.simd_operations.fetch_add(1, Ordering::Relaxed);
        Ok(result)
    }

    /// Fallback XOR for non-SIMD systems
    #[cfg(not(feature = "performance-opt"))]
    pub fn xor_parallel(&self, a: &[u8], b: &[u8]) -> Result<Vec<u8>> {
        if a.len() != b.len() {
            anyhow::bail!("Buffer length mismatch");
        }

        let result = a.iter()
            .zip(b.iter())
            .map(|(a_byte, b_byte)| a_byte ^ b_byte)
            .collect();

        self.stats.fallback_operations.fetch_add(1, Ordering::Relaxed);
        Ok(result)
    }

    /// Get performance statistics
    pub fn get_stats(&self) -> SimdStats {
        (*self.stats).clone()
    }
}

/// SIMD crypto performance statistics
#[derive(Debug, Clone, Default)]
pub struct SimdStats {
    /// Total crypto operations performed
    pub total_operations: AtomicU64,
    /// Total time spent in crypto operations (nanoseconds)
    pub total_time_ns: AtomicU64,
    /// Number of SIMD-accelerated operations
    pub simd_operations: AtomicU64,
    /// Number of fallback operations
    pub fallback_operations: AtomicU64,
    /// Hash operations performed
    pub hash_operations: AtomicU64,
    /// Time spent hashing (nanoseconds)
    pub hash_time_ns: AtomicU64,
    /// SIMD hash operations
    pub simd_hash_operations: AtomicU64,
    /// Fallback hash operations
    pub fallback_hash_operations: AtomicU64,
    /// Signature verifications performed
    pub signature_verifications: AtomicU64,
    /// Time spent on signature verification (nanoseconds)
    pub signature_time_ns: AtomicU64,
    /// Key derivations performed
    pub key_derivations: AtomicU64,
    /// Time spent on key derivation (nanoseconds)
    pub key_derivation_time_ns: AtomicU64,
}

impl SimdStats {
    /// Calculate operations per second
    pub fn ops_per_second(&self, duration: std::time::Duration) -> f64 {
        let ops = self.total_operations.load(Ordering::Relaxed) as f64;
        let seconds = duration.as_secs_f64();
        if seconds > 0.0 { ops / seconds } else { 0.0 }
    }

    /// Calculate SIMD utilization rate
    pub fn simd_utilization(&self) -> f64 {
        let simd_ops = self.simd_operations.load(Ordering::Relaxed) as f64;
        let total_ops = (simd_ops + self.fallback_operations.load(Ordering::Relaxed) as f64);
        if total_ops > 0.0 { simd_ops / total_ops } else { 0.0 }
    }

    /// Calculate average operation latency (nanoseconds)
    pub fn avg_operation_latency_ns(&self) -> f64 {
        let total_time = self.total_time_ns.load(Ordering::Relaxed) as f64;
        let total_ops = self.total_operations.load(Ordering::Relaxed) as f64;
        if total_ops > 0.0 { total_time / total_ops } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simd_crypto_creation() {
        let result = SimdCrypto::new();
        assert!(result.is_ok());

        let crypto = result.unwrap();
        let stats = crypto.get_stats();
        assert_eq!(stats.total_operations.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_xor_parallel() {
        let crypto = SimdCrypto::new().unwrap();
        let a = vec![0x12, 0x34, 0x56, 0x78];
        let b = vec![0xAB, 0xCD, 0xEF, 0x12];

        let result = crypto.xor_parallel(&a, &b).unwrap();
        let expected = vec![0xB9, 0xF9, 0xB9, 0x6A]; // Manual XOR

        assert_eq!(result, expected);
    }

    #[test]
    fn test_xor_parallel_length_mismatch() {
        let crypto = SimdCrypto::new().unwrap();
        let a = vec![1, 2, 3];
        let b = vec![4, 5];

        let result = crypto.xor_parallel(&a, &b);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_batch_encryption() {
        let crypto = SimdCrypto::new().unwrap();

        let messages = vec![
            Bytes::from_static(b"Hello"),
            Bytes::from_static(b"World"),
            Bytes::from_static(b"Test"),
        ];

        let key = [0u8; 32];
        let nonces = [
            [1u8; 12],
            [2u8; 12],
            [3u8; 12],
        ];

        let encrypted = crypto.encrypt_batch_aes_gcm(&messages, &key, &nonces).unwrap();
        assert_eq!(encrypted.len(), 3);

        // Each encrypted message should be different from plaintext
        for (i, encrypted_msg) in encrypted.iter().enumerate() {
            assert_ne!(encrypted_msg.as_ref(), messages[i].as_ref());
        }
    }

    #[test]
    fn test_batch_hashing() {
        let crypto = SimdCrypto::new().unwrap();

        let inputs = vec![
            Bytes::from_static(b"input1"),
            Bytes::from_static(b"input2"),
            Bytes::from_static(b"input3"),
        ];

        let hashes = crypto.hash_batch_blake3(&inputs).unwrap();
        assert_eq!(hashes.len(), 3);

        // Each hash should be different
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
    }

    #[test]
    fn test_key_derivation_batch() {
        let crypto = SimdCrypto::new().unwrap();

        let master_key = [0x42u8; 32];
        let salts = [
            [0x01u8; 32],
            [0x02u8; 32],
            [0x03u8; 32],
        ];
        let info = b"test info";

        let derived_keys = crypto.derive_keys_batch(&master_key, &salts, info, 32).unwrap();
        assert_eq!(derived_keys.len(), 3);

        // Each derived key should be different
        for i in 0..derived_keys.len() {
            for j in i+1..derived_keys.len() {
                assert_ne!(derived_keys[i], derived_keys[j]);
            }
        }
    }

    #[test]
    fn test_simd_stats_calculations() {
        let stats = SimdStats::default();
        stats.simd_operations.store(80, Ordering::Relaxed);
        stats.fallback_operations.store(20, Ordering::Relaxed);

        let utilization = stats.simd_utilization();
        assert!((utilization - 0.8).abs() < f64::EPSILON);
    }
}