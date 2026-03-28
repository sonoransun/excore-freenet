//! Contract garbage collection policy engine.
//!
//! Implements time-bounded cleanup policies that prevent unbounded storage growth.
//! All exemptions (active subscriptions, pending operations) are time-bounded
//! per AGENTS.md requirements -- no permanent GC blind spots.

use std::time::Duration;

use freenet_stdlib::prelude::ContractKey;
use serde::{Deserialize, Serialize};

/// Default storage quota for contract state: 500 MB.
pub const DEFAULT_STORAGE_QUOTA_BYTES: u64 = 500 * 1024 * 1024;

/// Default TTL for contracts that haven't been accessed: 7 days.
pub const DEFAULT_CONTRACT_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// Maximum age for any contract regardless of exemptions: 30 days.
/// This is the absolute age override that prevents permanent GC blind spots.
pub const DEFAULT_MAX_CONTRACT_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

/// Maximum TTL for GC exemptions (e.g., active subscriptions): 24 hours.
/// After this duration, exemptions expire and the contract becomes eligible for GC.
pub const DEFAULT_EXEMPTION_TTL: Duration = Duration::from_secs(24 * 3600);

/// Default interval between GC sweep runs: 5 minutes.
pub const DEFAULT_GC_INTERVAL: Duration = Duration::from_secs(300);

/// GC configuration for contract storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcConfig {
    /// Maximum total bytes for contract state storage.
    #[serde(default = "default_storage_quota")]
    pub storage_quota_bytes: u64,

    /// How long (seconds) a contract can remain without access before becoming GC-eligible.
    #[serde(default = "default_contract_ttl_secs")]
    pub contract_ttl_secs: u64,

    /// Absolute maximum age (seconds) for any contract, regardless of exemptions.
    /// This prevents permanent GC blind spots from unbounded exemptions.
    #[serde(default = "default_max_age_secs")]
    pub max_contract_age_secs: u64,

    /// Maximum duration (seconds) that a GC exemption can protect a contract.
    #[serde(default = "default_exemption_ttl_secs")]
    pub exemption_ttl_secs: u64,

    /// Interval (seconds) between automatic GC sweeps.
    #[serde(default = "default_gc_interval_secs")]
    pub gc_interval_secs: u64,

    /// Whether GC is enabled. Defaults to true.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_storage_quota() -> u64 {
    DEFAULT_STORAGE_QUOTA_BYTES
}
fn default_contract_ttl_secs() -> u64 {
    DEFAULT_CONTRACT_TTL.as_secs()
}
fn default_max_age_secs() -> u64 {
    DEFAULT_MAX_CONTRACT_AGE.as_secs()
}
fn default_exemption_ttl_secs() -> u64 {
    DEFAULT_EXEMPTION_TTL.as_secs()
}
fn default_gc_interval_secs() -> u64 {
    DEFAULT_GC_INTERVAL.as_secs()
}
fn default_enabled() -> bool {
    true
}

impl GcConfig {
    /// Get the contract TTL as a Duration.
    pub fn contract_ttl(&self) -> Duration {
        Duration::from_secs(self.contract_ttl_secs)
    }

    /// Get the max contract age as a Duration.
    pub fn max_contract_age(&self) -> Duration {
        Duration::from_secs(self.max_contract_age_secs)
    }

    /// Get the exemption TTL as a Duration.
    pub fn exemption_ttl(&self) -> Duration {
        Duration::from_secs(self.exemption_ttl_secs)
    }

    /// Get the GC interval as a Duration.
    pub fn gc_interval(&self) -> Duration {
        Duration::from_secs(self.gc_interval_secs)
    }
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            storage_quota_bytes: DEFAULT_STORAGE_QUOTA_BYTES,
            contract_ttl_secs: DEFAULT_CONTRACT_TTL.as_secs(),
            max_contract_age_secs: DEFAULT_MAX_CONTRACT_AGE.as_secs(),
            exemption_ttl_secs: DEFAULT_EXEMPTION_TTL.as_secs(),
            gc_interval_secs: DEFAULT_GC_INTERVAL.as_secs(),
            enabled: true,
        }
    }
}

/// Result of a GC sweep operation.
#[derive(Debug, Default)]
pub struct GcSweepResult {
    /// Contracts removed during this sweep.
    pub removed: Vec<ContractKey>,
    /// Contracts that were exempt but whose exemption was time-bounded.
    pub exempted: usize,
    /// Total bytes freed.
    pub bytes_freed: u64,
    /// Errors encountered during removal (non-fatal).
    pub errors: Vec<(ContractKey, String)>,
}

/// GC policy that determines whether a contract should be collected.
///
/// All exemptions are time-bounded: even if a contract has active subscriptions
/// or pending operations, it will be collected after `max_contract_age`.
pub struct GcPolicy {
    config: GcConfig,
}

impl GcPolicy {
    pub fn new(config: GcConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &GcConfig {
        &self.config
    }

    /// Determine whether a contract should be collected.
    ///
    /// # Arguments
    /// * `last_access_age` - Time since the contract was last accessed
    /// * `contract_age` - Total age of the contract since first stored
    /// * `has_active_exemption` - Whether the contract has an active exemption
    ///   (e.g., active subscription, pending operation)
    /// * `exemption_age` - How long the current exemption has been active
    pub fn should_collect(
        &self,
        last_access_age: Duration,
        contract_age: Duration,
        has_active_exemption: bool,
        exemption_age: Option<Duration>,
    ) -> bool {
        if !self.config.enabled {
            return false;
        }

        // Absolute age override: collect regardless of exemptions.
        // This is the critical safeguard against permanent GC blind spots.
        let max_age = self.config.max_contract_age();
        if contract_age >= max_age {
            tracing::debug!(
                contract_age_secs = contract_age.as_secs(),
                max_age_secs = max_age.as_secs(),
                "Contract exceeds max age, collecting despite any exemptions"
            );
            return true;
        }

        // If actively exempt and exemption hasn't expired, skip collection
        if has_active_exemption {
            let ex_ttl = self.config.exemption_ttl();
            if let Some(ex_age) = exemption_age {
                if ex_age < ex_ttl {
                    return false;
                }
                // Exemption has expired -- fall through to normal TTL check
                tracing::debug!(
                    exemption_age_secs = ex_age.as_secs(),
                    exemption_ttl_secs = ex_ttl.as_secs(),
                    "GC exemption expired, contract now eligible for collection"
                );
            }
        }

        // Normal TTL-based collection
        last_access_age >= self.config.contract_ttl()
    }

    /// Check if storage is over quota and GC should run aggressively.
    pub fn is_over_quota(&self, current_bytes: u64) -> bool {
        current_bytes > self.config.storage_quota_bytes
    }

    /// Get the fraction of quota currently used (0.0 to 1.0+).
    pub fn quota_usage(&self, current_bytes: u64) -> f64 {
        if self.config.storage_quota_bytes == 0 {
            return f64::INFINITY;
        }
        current_bytes as f64 / self.config.storage_quota_bytes as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_policy() -> GcPolicy {
        GcPolicy::new(GcConfig::default())
    }

    #[test]
    fn test_collect_when_past_ttl() {
        let policy = default_policy();
        let last_access = DEFAULT_CONTRACT_TTL + Duration::from_secs(1);
        let contract_age = last_access;
        assert!(policy.should_collect(last_access, contract_age, false, None));
    }

    #[test]
    fn test_no_collect_when_recently_accessed() {
        let policy = default_policy();
        let last_access = Duration::from_secs(60);
        let contract_age = Duration::from_secs(3600);
        assert!(!policy.should_collect(last_access, contract_age, false, None));
    }

    #[test]
    fn test_exemption_protects_temporarily() {
        let policy = default_policy();
        let last_access = DEFAULT_CONTRACT_TTL + Duration::from_secs(1);
        let contract_age = last_access;
        let exemption_age = Duration::from_secs(60); // Fresh exemption
        assert!(!policy.should_collect(last_access, contract_age, true, Some(exemption_age)));
    }

    #[test]
    fn test_expired_exemption_allows_collection() {
        let policy = default_policy();
        let last_access = DEFAULT_CONTRACT_TTL + Duration::from_secs(1);
        let contract_age = last_access;
        // Exemption older than exemption_ttl
        let exemption_age = DEFAULT_EXEMPTION_TTL + Duration::from_secs(1);
        assert!(policy.should_collect(last_access, contract_age, true, Some(exemption_age)));
    }

    #[test]
    fn test_max_age_override_ignores_exemptions() {
        let policy = default_policy();
        let contract_age = DEFAULT_MAX_CONTRACT_AGE + Duration::from_secs(1);
        let last_access = Duration::from_secs(0); // Just accessed
        let exemption_age = Duration::from_secs(0); // Fresh exemption
                                                    // Even with a fresh exemption and recent access, max age forces collection
        assert!(policy.should_collect(last_access, contract_age, true, Some(exemption_age)));
    }

    #[test]
    fn test_disabled_gc_never_collects() {
        let config = GcConfig {
            enabled: false,
            ..Default::default()
        };
        let policy = GcPolicy::new(config);
        let old_age = DEFAULT_MAX_CONTRACT_AGE + Duration::from_secs(3600);
        assert!(!policy.should_collect(old_age, old_age, false, None));
    }

    #[test]
    fn test_quota_usage() {
        let policy = default_policy();
        assert!(!policy.is_over_quota(DEFAULT_STORAGE_QUOTA_BYTES / 2));
        assert!(policy.is_over_quota(DEFAULT_STORAGE_QUOTA_BYTES + 1));

        let usage = policy.quota_usage(DEFAULT_STORAGE_QUOTA_BYTES / 2);
        assert!((usage - 0.5).abs() < 0.001);
    }
}
