//! Background maintenance tasks for the Freenet node.
//!
//! Provides three long-lived background tasks:
//!
//! - [`seeding_task`]: Periodically re-PUTs hosted contracts to ensure they remain
//!   replicated in the network near their location after peer churn.
//!
//! - [`storage_verification_task`]: Periodically reads locally stored contract states
//!   and checks their integrity. Issues network GETs to recover corrupted or missing data.
//!
//! - [`durable_operation_task`]: Accepts [`DurableOpRequest`] messages and retries
//!   registered GET/SUBSCRIBE operations with exponential backoff until success or the
//!   configured [`MaintenanceConfig::max_durable_retries`] limit is reached.
//!
//! All tasks follow the patterns established in
//! [`crate::ring::Ring::recover_orphaned_subscriptions`]:
//! wait for ring connections before starting, use [`GlobalExecutor::spawn`], use
//! [`GlobalRng`] for jitter, and never use raw `tokio::spawn`.

use std::{collections::HashMap, sync::Arc, time::Duration};

use dashmap::DashSet;
use freenet_stdlib::prelude::{ContractInstanceId, ContractKey, RelatedContracts};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{
    config::{GlobalExecutor, GlobalRng},
    contract::{ContractHandlerEvent, StoreResponse},
    operations::{get, put, subscribe, VisitedPeers},
    util::backoff::{ExponentialBackoff, TrackedBackoff},
};

use super::{DurableOpKind, DurableOpRequest, OpManager};

/// Maximum consecutive verification failures before evicting a contract.
const MAX_VERIFICATION_FAILURES: u32 = 3;

/// Maximum durable operations dispatched per sweep cycle (prevents channel saturation).
const MAX_DURABLE_RETRIES_PER_TICK: usize = 10;

/// Maximum allowed contract state size (50 MiB, matches storage layer enforcement).
const MAX_CONTRACT_STATE_SIZE: usize = 50 * 1024 * 1024;

/// PUT hops-to-live for re-seeding (= DEFAULT_MAX_BREADTH, limits network blast radius).
const REPLICATION_HTL: usize = 3;

/// Maximum time to wait for the first ring connection before starting maintenance.
const MAX_WAIT_FOR_CONNECTION: Duration = Duration::from_secs(300);

fn default_seeding_interval_secs() -> u64 {
    300
}

fn default_seeding_batch_size() -> usize {
    20
}

fn default_verification_interval_secs() -> u64 {
    600
}

fn default_verification_batch_size() -> usize {
    20
}

fn default_max_durable_retries() -> u32 {
    10
}

fn default_maintenance_enabled() -> bool {
    true
}

/// Configuration for node background maintenance tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceConfig {
    /// Interval in seconds between seeding cycles. Default: 300s (5 min).
    #[serde(default = "default_seeding_interval_secs")]
    pub seeding_interval_secs: u64,

    /// Maximum contracts to re-seed per cycle. Default: 20.
    #[serde(default = "default_seeding_batch_size")]
    pub seeding_batch_size: usize,

    /// Interval in seconds between storage verification sweeps. Default: 600s (10 min).
    #[serde(default = "default_verification_interval_secs")]
    pub verification_interval_secs: u64,

    /// Maximum contracts to verify per sweep. Default: 20.
    #[serde(default = "default_verification_batch_size")]
    pub verification_batch_size: usize,

    /// Maximum retry attempts for a durable operation before exhaustion. Default: 10.
    #[serde(default = "default_max_durable_retries")]
    pub max_durable_retries: u32,

    /// Whether all background maintenance tasks are enabled. Default: true.
    #[serde(default = "default_maintenance_enabled")]
    pub enabled: bool,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            seeding_interval_secs: default_seeding_interval_secs(),
            seeding_batch_size: default_seeding_batch_size(),
            verification_interval_secs: default_verification_interval_secs(),
            verification_batch_size: default_verification_batch_size(),
            max_durable_retries: default_max_durable_retries(),
            enabled: default_maintenance_enabled(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Durable operation registry (owned by durable_operation_task)
// ─────────────────────────────────────────────────────────────────────────────

struct DurableOp {
    instance_id: ContractInstanceId,
    kind: DurableOpKind,
    attempts: u32,
    retry_after: tokio::time::Instant,
    fallback: Option<DurableOpKind>,
}

struct DurableOpRegistry {
    ops: HashMap<ContractInstanceId, DurableOp>,
    backoff: ExponentialBackoff,
    max_retries: u32,
}

impl DurableOpRegistry {
    fn new(max_retries: u32) -> Self {
        Self {
            ops: HashMap::new(),
            // 5s base, 5min max — same as ExponentialBackoff::default()
            backoff: ExponentialBackoff::default(),
            max_retries,
        }
    }

    fn handle(&mut self, req: DurableOpRequest) {
        match req {
            DurableOpRequest::Register {
                instance_id,
                kind,
                fallback,
            } => {
                use std::collections::hash_map::Entry;
                if let Entry::Vacant(e) = self.ops.entry(instance_id) {
                    tracing::info!(
                        contract = %instance_id,
                        kind = ?kind,
                        has_fallback = fallback.is_some(),
                        "Durable operation registered"
                    );
                    e.insert(DurableOp {
                        instance_id,
                        kind,
                        attempts: 0,
                        retry_after: tokio::time::Instant::now(),
                        fallback,
                    });
                }
            }
            DurableOpRequest::Cancel { instance_id } => {
                if self.ops.remove(&instance_id).is_some() {
                    tracing::info!(
                        contract = %instance_id,
                        "Durable operation cancelled"
                    );
                }
            }
            DurableOpRequest::Complete { instance_id } => {
                if self.ops.remove(&instance_id).is_some() {
                    tracing::info!(
                        contract = %instance_id,
                        "Durable operation completed successfully"
                    );
                }
            }
        }
    }

    async fn sweep(&mut self, op_manager: &Arc<OpManager>) {
        let now = tokio::time::Instant::now();
        let max_retries = self.max_retries;

        // Collect IDs of operations ready to be dispatched this sweep.
        let ready: Vec<ContractInstanceId> = self
            .ops
            .iter()
            .filter(|(_, op)| now >= op.retry_after)
            .map(|(id, _)| *id)
            .take(MAX_DURABLE_RETRIES_PER_TICK)
            .collect();

        let mut to_remove: Vec<ContractInstanceId> = Vec::new();
        let mut to_fallback: Vec<ContractInstanceId> = Vec::new();
        let mut dispatched = 0usize;

        for id in ready {
            if dispatched >= MAX_DURABLE_RETRIES_PER_TICK {
                break;
            }

            let Some(op) = self.ops.get_mut(&id) else {
                continue;
            };

            let instance_id = op.instance_id;
            let op_mgr = op_manager.clone();
            let attempt = op.attempts + 1;

            match &op.kind {
                DurableOpKind::Get { fetch_contract } => {
                    let fetch_contract = *fetch_contract;
                    GlobalExecutor::spawn(async move {
                        let get_op = get::start_op(instance_id, fetch_contract, false, false);
                        let visited = VisitedPeers::new(&get_op.id);
                        match get::request_get(&op_mgr, get_op, visited).await {
                            Ok(()) => {
                                tracing::info!(
                                    contract = %instance_id,
                                    attempt,
                                    "Durable GET succeeded"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    contract = %instance_id,
                                    attempt,
                                    error = %e,
                                    "Durable GET attempt failed"
                                );
                            }
                        }
                    });
                }
                DurableOpKind::Subscribe { is_renewal } => {
                    let is_renewal = *is_renewal;
                    GlobalExecutor::spawn(async move {
                        let sub_op = subscribe::start_op(instance_id, is_renewal);
                        match subscribe::request_subscribe(&op_mgr, sub_op).await {
                            Ok(()) => {
                                tracing::info!(
                                    contract = %instance_id,
                                    attempt,
                                    "Durable SUBSCRIBE succeeded"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    contract = %instance_id,
                                    attempt,
                                    error = %e,
                                    "Durable SUBSCRIBE attempt failed"
                                );
                            }
                        }
                    });
                }
            }

            op.attempts += 1;
            dispatched += 1;
            op.retry_after = now + self.backoff.delay(op.attempts);

            if op.attempts >= max_retries {
                if op.fallback.is_some() {
                    to_fallback.push(id);
                } else {
                    tracing::warn!(
                        contract = %id,
                        attempts = op.attempts,
                        "Durable operation exhausted all retries without success"
                    );
                    to_remove.push(id);
                }
            }
        }

        let exhausted_count = to_remove.len();
        let fallback_count = to_fallback.len();

        for id in to_remove {
            self.ops.remove(&id);
        }

        for id in to_fallback {
            if let Some(op) = self.ops.get_mut(&id) {
                if let Some(fallback_kind) = op.fallback.take() {
                    tracing::info!(
                        contract = %id,
                        fallback_kind = ?fallback_kind,
                        "Durable operation primary exhausted, switching to fallback"
                    );
                    op.kind = fallback_kind;
                    op.attempts = 0;
                    op.retry_after = tokio::time::Instant::now();
                } else {
                    self.ops.remove(&id);
                }
            }
        }

        if dispatched > 0 || exhausted_count > 0 || fallback_count > 0 {
            tracing::info!(
                dispatched,
                exhausted = exhausted_count,
                switched_to_fallback = fallback_count,
                pending = self.ops.len(),
                "Durable operation sweep complete"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Connection wait helper
// ─────────────────────────────────────────────────────────────────────────────

/// Wait until the ring has at least one connection (or the startup timeout elapses).
///
/// Mirrors the pattern in `Ring::recover_orphaned_subscriptions`.
async fn wait_for_ring_connection(op_manager: &Arc<OpManager>) {
    let wait_start = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if op_manager.ring.open_connections() > 0 {
            tracing::info!("Maintenance: ring connection established, starting tasks");
            break;
        }
        if wait_start.elapsed() >= MAX_WAIT_FOR_CONNECTION {
            tracing::warn!(
                "Maintenance: no ring connections after {:?}, starting anyway",
                MAX_WAIT_FOR_CONNECTION
            );
            break;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 1: Seeding
// ─────────────────────────────────────────────────────────────────────────────

/// Periodically re-PUTs hosted contracts to maintain network replication.
///
/// Without this, contracts can silently drop out of the ring after peer churn:
/// peers near the contract's location disconnect and the remaining copies become
/// harder to discover before new peers learn about the contract.
///
/// For each contract in the hosting cache (shuffled, batched), the task:
/// 1. Skips if in exponential backoff (from a previous handler error).
/// 2. Skips if a seeding PUT is already in flight for this contract.
/// 3. Fetches state + contract code from local storage.
/// 4. If available, spawns a PUT with HTL = `REPLICATION_HTL` (= 3).
pub(crate) async fn seeding_task(op_manager: Arc<OpManager>, config: MaintenanceConfig) {
    wait_for_ring_connection(&op_manager).await;

    // Startup jitter to avoid thundering herd when many nodes restart together.
    let jitter = Duration::from_secs(GlobalRng::random_range(10u64..=30u64));
    tokio::time::sleep(jitter).await;

    let mut seeding_backoff: TrackedBackoff<ContractKey> = TrackedBackoff::new(
        ExponentialBackoff::new(Duration::from_secs(60), Duration::from_secs(1800)),
        512,
    );

    // Set of contracts that have a PUT in flight — prevents duplicate spawns.
    let pending: Arc<DashSet<ContractKey>> = Arc::new(DashSet::new());

    let interval = Duration::from_secs(config.seeding_interval_secs);
    let mut tick = tokio::time::interval(interval);
    tick.tick().await; // skip the immediate first tick

    loop {
        tick.tick().await;

        if op_manager.ring.open_connections() == 0 {
            tracing::debug!("Seeding task: no ring connections, deferring cycle");
            continue;
        }

        // Backpressure: skip if the event loop notification channel is >75% full.
        // Seeding is background work; starving real operations is worse.
        let sender = op_manager.to_event_listener.notifications_sender();
        if sender.capacity() < sender.max_capacity() / 4 {
            tracing::debug!("Seeding task: notification channel congested, deferring cycle");
            continue;
        }

        let mut keys = op_manager.ring.hosting_contract_keys();
        GlobalRng::shuffle(&mut keys);
        let batch: Vec<ContractKey> = keys.into_iter().take(config.seeding_batch_size).collect();

        seeding_backoff.cleanup_expired();

        tracing::debug!(batch_size = batch.len(), "Seeding cycle starting");

        let mut spawned = 0u32;
        let mut skipped_backoff = 0u32;
        let mut skipped_pending = 0u32;
        let mut skipped_unavailable = 0u32;
        let mut handler_errors = 0u32;

        for key in batch {
            if seeding_backoff.is_in_backoff(&key) {
                skipped_backoff += 1;
                continue;
            }
            if pending.contains(&key) {
                skipped_pending += 1;
                continue;
            }

            let instance_id = *key.id();

            match op_manager
                .notify_contract_handler(ContractHandlerEvent::GetQuery {
                    instance_id,
                    return_contract_code: true,
                })
                .await
            {
                Ok(ContractHandlerEvent::GetResponse { response, .. }) => match response {
                    Ok(StoreResponse {
                        state: Some(state),
                        contract: Some(contract),
                    }) => {
                        pending.insert(key.clone());
                        let op_mgr = op_manager.clone();
                        let pending_clone = pending.clone();
                        let key_clone = key;
                        spawned += 1;

                        GlobalExecutor::spawn(async move {
                            let put_op = put::start_op(
                                contract,
                                RelatedContracts::default(),
                                state,
                                REPLICATION_HTL,
                                false,
                                false,
                            );
                            match put::request_put(&op_mgr, put_op).await {
                                Ok(()) => {
                                    tracing::info!(
                                        contract = %key_clone,
                                        "Seeding PUT completed successfully"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        contract = %key_clone,
                                        error = %e,
                                        "Seeding PUT failed (will retry next cycle)"
                                    );
                                }
                            }
                            pending_clone.remove(&key_clone);
                        });
                    }
                    Ok(_) => {
                        skipped_unavailable += 1;
                    }
                    Err(e) => {
                        tracing::warn!(
                            contract = %key,
                            error = %e,
                            "Seeding: contract handler error, applying backoff"
                        );
                        handler_errors += 1;
                        seeding_backoff.record_failure(key);
                    }
                },
                Ok(_) | Err(_) => {
                    handler_errors += 1;
                    seeding_backoff.record_failure(key);
                }
            }
        }

        tracing::info!(
            spawned,
            skipped_backoff,
            skipped_pending,
            skipped_unavailable,
            handler_errors,
            "Seeding cycle complete"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 2: Storage Verification
// ─────────────────────────────────────────────────────────────────────────────

/// Periodically verifies locally stored contract states for integrity.
///
/// For each contract in the hosting cache (shuffled, batched), the task:
/// 1. Skips if in backoff (a recovery GET was recently triggered for this contract).
/// 2. Issues a local [`ContractHandlerEvent::GetQuery`] to read the stored state.
/// 3. Considers the state **healthy** if: present, non-empty, ≤ 50 MiB.
/// 4. On failure: applies backoff, spawns a network GET to recover fresh state.
/// 5. After [`MAX_VERIFICATION_FAILURES`] consecutive failures: evicts the contract.
pub(crate) async fn storage_verification_task(
    op_manager: Arc<OpManager>,
    config: MaintenanceConfig,
) {
    wait_for_ring_connection(&op_manager).await;

    let jitter = Duration::from_secs(GlobalRng::random_range(5u64..=20u64));
    tokio::time::sleep(jitter).await;

    let mut verification_backoff: TrackedBackoff<ContractKey> = TrackedBackoff::new(
        ExponentialBackoff::new(Duration::from_secs(30), Duration::from_secs(900)),
        512,
    );
    let mut failure_counts: HashMap<ContractKey, u32> = HashMap::new();

    let interval = Duration::from_secs(config.verification_interval_secs);
    let mut tick = tokio::time::interval(interval);
    tick.tick().await; // skip immediate

    loop {
        tick.tick().await;

        let mut keys = op_manager.ring.hosting_contract_keys();
        GlobalRng::shuffle(&mut keys);
        let batch: Vec<ContractKey> = keys.into_iter().take(config.verification_batch_size).collect();

        verification_backoff.cleanup_expired();

        tracing::debug!(batch_size = batch.len(), "Verification cycle starting");

        let mut healthy_count = 0u32;
        let mut failed_count = 0u32;
        let mut evicted_count = 0u32;
        let mut recovery_started = 0u32;

        for key in batch {
            if verification_backoff.is_in_backoff(&key) {
                continue;
            }

            let instance_id = *key.id();

            let result = op_manager
                .notify_contract_handler(ContractHandlerEvent::GetQuery {
                    instance_id,
                    return_contract_code: false,
                })
                .await;

            let healthy = match result {
                Ok(ContractHandlerEvent::GetResponse { response, .. }) => match response {
                    Ok(StoreResponse {
                        state: Some(state), ..
                    }) => {
                        let size = state.as_ref().len();
                        size > 0 && size <= MAX_CONTRACT_STATE_SIZE
                    }
                    Ok(StoreResponse { state: None, .. }) => {
                        // Contract not in local store (LRU evicted) — not an error.
                        continue;
                    }
                    Err(_) => false,
                },
                Ok(_) | Err(_) => false,
            };

            if healthy {
                healthy_count += 1;
                failure_counts.remove(&key);
                verification_backoff.record_success(&key);
            } else {
                failed_count += 1;
                let count_val = {
                    let count = failure_counts.entry(key.clone()).or_insert(0);
                    *count += 1;
                    *count
                };

                verification_backoff.record_failure(key.clone());

                tracing::warn!(
                    contract = %key,
                    consecutive_failures = count_val,
                    "Storage verification failed"
                );

                if count_val >= MAX_VERIFICATION_FAILURES {
                    let bytes_freed = op_manager.ring.evict_hosted_contract(&key);
                    tracing::error!(
                        contract = %key,
                        bytes_freed,
                        "Contract evicted after persistent storage verification failures"
                    );
                    failure_counts.remove(&key);
                    evicted_count += 1;
                } else {
                    recovery_started += 1;
                    let op_mgr = op_manager.clone();
                    GlobalExecutor::spawn(async move {
                        let get_op = get::start_op(instance_id, false, false, false);
                        let visited = VisitedPeers::new(&get_op.id);
                        match get::request_get(&op_mgr, get_op, visited).await {
                            Ok(()) => {
                                tracing::info!(
                                    contract = %instance_id,
                                    "Verification recovery GET succeeded"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    contract = %instance_id,
                                    error = %e,
                                    "Verification recovery GET failed"
                                );
                            }
                        }
                    });
                }
            }
        }

        tracing::info!(
            healthy = healthy_count,
            failed = failed_count,
            evicted = evicted_count,
            recovery_started,
            "Verification cycle complete"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Task 3: Durable operation processing
// ─────────────────────────────────────────────────────────────────────────────

/// Processes [`DurableOpRequest`] messages, retrying GET/SUBSCRIBE operations
/// with exponential backoff until success or exhaustion.
///
/// The sender half is stored on [`OpManager`] via [`OpManager::set_durable_op_sender`],
/// allowing any subsystem to enqueue must-complete operations without coupling to
/// this module directly.
///
/// Fallback: if the primary operation kind exhausts `max_retries`, it is replaced
/// by the registered fallback kind (if any) with a fresh attempt counter.
pub(crate) async fn durable_operation_task(
    op_manager: Arc<OpManager>,
    mut requests: mpsc::Receiver<DurableOpRequest>,
    max_retries: u32,
) {
    let mut registry = DurableOpRegistry::new(max_retries);
    let mut sweep_tick = tokio::time::interval(Duration::from_secs(5));
    sweep_tick.tick().await; // skip immediate

    loop {
        crate::deterministic_select! {
            req = requests.recv() => {
                match req {
                    Some(req) => registry.handle(req),
                    None => {
                        tracing::info!("Durable operation task: channel closed, exiting");
                        break;
                    }
                }
            },
            _ = sweep_tick.tick() => {
                registry.sweep(&op_manager).await;
            },
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_maintenance_config_defaults() {
        let config = MaintenanceConfig::default();
        assert!(config.enabled);
        assert_eq!(config.seeding_interval_secs, 300);
        assert_eq!(config.seeding_batch_size, 20);
        assert_eq!(config.verification_interval_secs, 600);
        assert_eq!(config.max_durable_retries, 10);
    }

    #[test]
    fn test_durable_registry_cancel_removes_op() {
        let id = ContractInstanceId::new([1u8; 32]);
        let mut registry = DurableOpRegistry::new(3);

        registry.handle(DurableOpRequest::Register {
            instance_id: id,
            kind: DurableOpKind::Get {
                fetch_contract: false,
            },
            fallback: None,
        });
        assert!(registry.ops.contains_key(&id));

        registry.handle(DurableOpRequest::Cancel { instance_id: id });
        assert!(!registry.ops.contains_key(&id), "cancel should remove the op");
    }

    #[test]
    fn test_durable_registry_complete_removes_op() {
        let id = ContractInstanceId::new([2u8; 32]);
        let mut registry = DurableOpRegistry::new(3);

        registry.handle(DurableOpRequest::Register {
            instance_id: id,
            kind: DurableOpKind::Subscribe { is_renewal: true },
            fallback: None,
        });
        assert!(registry.ops.contains_key(&id));

        registry.handle(DurableOpRequest::Complete { instance_id: id });
        assert!(!registry.ops.contains_key(&id), "complete should remove the op");
    }

    #[test]
    fn test_durable_registry_no_duplicate_registration() {
        let id = ContractInstanceId::new([3u8; 32]);
        let mut registry = DurableOpRegistry::new(3);

        // First registration
        registry.handle(DurableOpRequest::Register {
            instance_id: id,
            kind: DurableOpKind::Get { fetch_contract: true },
            fallback: None,
        });

        // Simulate some progress
        if let Some(op) = registry.ops.get_mut(&id) {
            op.attempts = 2;
        }

        // Second registration should not overwrite
        registry.handle(DurableOpRequest::Register {
            instance_id: id,
            kind: DurableOpKind::Subscribe { is_renewal: false },
            fallback: None,
        });

        let op = registry.ops.get(&id).unwrap();
        assert_eq!(op.attempts, 2, "second registration should not reset attempts");
        assert!(
            matches!(op.kind, DurableOpKind::Get { .. }),
            "second registration should not overwrite kind"
        );
    }

    #[test]
    fn test_durable_registry_fallback_transition() {
        let id = ContractInstanceId::new([4u8; 32]);
        let max_retries = 2u32;
        let mut registry = DurableOpRegistry::new(max_retries);

        registry.handle(DurableOpRequest::Register {
            instance_id: id,
            kind: DurableOpKind::Get { fetch_contract: false },
            fallback: Some(DurableOpKind::Subscribe { is_renewal: false }),
        });

        // Manually transition to fallback (mirrors what sweep does on exhaustion)
        if let Some(op) = registry.ops.get_mut(&id) {
            op.attempts = max_retries;
            if let Some(fallback_kind) = op.fallback.take() {
                op.kind = fallback_kind;
                op.attempts = 0;
                op.retry_after = tokio::time::Instant::now();
            }
        }

        let op = registry.ops.get(&id).unwrap();
        assert!(
            matches!(op.kind, DurableOpKind::Subscribe { is_renewal: false }),
            "should have transitioned to fallback Subscribe"
        );
        assert_eq!(op.attempts, 0, "fallback should reset attempt count");
        assert!(op.fallback.is_none(), "fallback should be consumed");
    }
}
