//! Contract deletion: explicit removal of contracts from local storage.
//!
//! Provides the `ContractDeletion` struct that coordinates removal across all
//! storage layers (state store, contract store, hosting cache, subscriptions).

use freenet_stdlib::prelude::ContractKey;

/// Result of a contract deletion operation.
#[derive(Debug)]
pub struct DeletionResult {
    /// The contract key that was deleted.
    pub key: ContractKey,
    /// Whether the contract existed before deletion.
    pub existed: bool,
    /// Bytes freed by the deletion.
    pub bytes_freed: u64,
}

/// Coordinates contract deletion across storage layers.
///
/// Deletion order is critical to avoid inconsistencies:
/// 1. Remove from hosting cache (stops serving to network)
/// 2. Remove subscriber notifications (stops sending updates)
/// 3. Remove state from persistent storage
/// 4. Remove contract WASM from contract store
/// 5. Remove hosting metadata from persistent storage
pub struct ContractDeletion;

impl ContractDeletion {
    /// Validate that a contract key is eligible for deletion.
    ///
    /// Returns an error message if the contract cannot be deleted.
    pub fn validate_deletion(key: &ContractKey) -> Result<(), String> {
        // Validate the key is not a zero/null key
        if key.as_bytes().iter().all(|&b| b == 0) {
            return Err("Cannot delete null contract key".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use freenet_stdlib::prelude::*;

    fn make_test_key() -> ContractKey {
        let code = ContractCode::from(vec![1, 2, 3, 4]);
        let params = Parameters::from(vec![5, 6, 7, 8]);
        ContractKey::from_params_and_code(&params, &code)
    }

    #[test]
    fn test_validate_deletion_accepts_valid_key() {
        let key = make_test_key();
        assert!(ContractDeletion::validate_deletion(&key).is_ok());
    }
}
