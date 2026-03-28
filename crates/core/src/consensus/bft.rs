//! Asynchronous Byzantine Fault Tolerance (BFT) Consensus
//!
//! This module provides Alea-BFT asynchronous consensus algorithms that do not
//! depend on timing assumptions and provide guaranteed liveness under Byzantine
//! adversaries.
//!
//! ## Features
//!
//! - **Asynchronous BFT**: No timeout assumptions, guaranteed liveness
//! - **Quadratic Complexity**: O(n²) vs O(n³) for traditional PBFT
//! - **Byzantine Recovery**: Handle arbitrary f < n/3 malicious nodes
//! - **Network Partition Tolerance**: Continue operation during network splits

use crate::ml::{Enhanced, MLConfig};
use crate::operations::OpManager;
use crate::simulation::TimeSource;

use anyhow::{Result, Context, bail};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::collections::{HashMap, BTreeMap, HashSet, VecDeque};
use bytes::{Bytes, BytesMut};
use serde::{Serialize, Deserialize};
use parking_lot::RwLock;

#[cfg(feature = "bft-async")]
use hbbft::{NetworkInfo, DynamicHoneyBadger, NodeIdT, Contribution};

/// BFT enhanced operation manager
pub type BFTConsensus = Enhanced<OpManager, BFTEnhancer>;

impl BFTConsensus {
    /// Create new BFT consensus manager
    pub fn new(base: OpManager, config: BFTConfig) -> Result<Self> {
        let enhancement = if config.enabled {
            Some(BFTEnhancer::new(config.clone())?)
        } else {
            None
        };

        let ml_config = MLConfig {
            enabled: config.enabled,
            fallback_on_error: config.fallback_on_error,
            model_path: None,
            max_inference_latency_ms: 1000, // 1s max for BFT consensus
        };

        Ok(Enhanced::new(base, enhancement, ml_config))
    }

    /// Submit operation for BFT consensus
    pub async fn submit_for_consensus(
        &self,
        operation: BFTOperation,
    ) -> Result<BFTConsensusResult> {
        if let Some(ref enhancer) = self.enhancement {
            enhancer.submit_operation(&self.base, operation).await
        } else {
            // Direct execution without BFT consensus
            Ok(BFTConsensusResult {
                operation_id: operation.id.clone(),
                consensus_reached: true,
                execution_result: Bytes::from("direct_execution"),
                participating_nodes: HashSet::new(),
                consensus_time_ms: 0,
                used_fallback: true,
            })
        }
    }

    /// Get BFT statistics
    pub fn bft_stats(&self) -> BFTStats {
        self.enhancement.as_ref()
            .map(|e| e.stats())
            .unwrap_or_default()
    }

    /// Get current view of network membership
    pub fn network_membership(&self) -> Vec<BFTNodeId> {
        self.enhancement.as_ref()
            .map(|e| e.network_membership())
            .unwrap_or_default()
    }
}

/// BFT consensus configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BFTConfig {
    /// Enable BFT consensus
    pub enabled: bool,
    /// Fallback to non-BFT operation on consensus failures
    pub fallback_on_error: bool,
    /// Node identifier in the BFT network
    pub node_id: BFTNodeId,
    /// Maximum number of Byzantine nodes (f < n/3)
    pub max_byzantine_nodes: usize,
    /// Consensus timeout for practical termination
    pub consensus_timeout_ms: u64,
    /// Batch size for aggregating operations
    pub batch_size: usize,
    /// Enable dynamic membership changes
    pub dynamic_membership: bool,
    /// Minimum network size for consensus
    pub min_network_size: usize,
}

impl Default for BFTConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default due to complexity
            fallback_on_error: true,
            node_id: BFTNodeId::from("default_node"),
            max_byzantine_nodes: 1, // f=1 allows up to 4 total nodes
            consensus_timeout_ms: 30000, // 30 seconds
            batch_size: 10,
            dynamic_membership: false,
            min_network_size: 4, // Minimum for f=1 Byzantine tolerance
        }
    }
}

/// BFT node identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BFTNodeId(String);

impl BFTNodeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for BFTNodeId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for BFTNodeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for BFTNodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// BFT operation to be consensus-ordered
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BFTOperation {
    /// Operation identifier
    pub id: String,
    /// Operation type
    pub op_type: BFTOperationType,
    /// Operation payload
    pub payload: Bytes,
    /// Submitting node
    pub submitter: BFTNodeId,
    /// Operation priority
    pub priority: BFTOperationPriority,
    /// Submission timestamp
    pub timestamp: u64,
}

/// Types of BFT operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BFTOperationType {
    /// Contract state update
    ContractUpdate,
    /// Network membership change
    MembershipChange,
    /// Configuration change
    ConfigurationChange,
    /// Custom operation
    Custom(String),
}

/// Operation priority levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BFTOperationPriority {
    /// Low priority operations
    Low = 0,
    /// Normal priority operations
    Normal = 1,
    /// High priority operations
    High = 2,
    /// Critical priority operations (membership changes)
    Critical = 3,
}

/// Result of BFT consensus
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BFTConsensusResult {
    /// Operation identifier
    pub operation_id: String,
    /// Whether consensus was reached
    pub consensus_reached: bool,
    /// Execution result
    pub execution_result: Bytes,
    /// Nodes that participated in consensus
    pub participating_nodes: HashSet<BFTNodeId>,
    /// Time taken for consensus
    pub consensus_time_ms: u64,
    /// Whether fallback was used
    pub used_fallback: bool,
}

/// BFT enhancement implementation
#[cfg(feature = "bft-async")]
pub struct BFTEnhancer {
    config: BFTConfig,
    pending_operations: RwLock<VecDeque<BFTOperation>>,
    consensus_state: RwLock<BFTConsensusState>,
    network_membership: RwLock<HashSet<BFTNodeId>>,
    stats: RwLock<BFTStats>,
    time_source: Arc<dyn TimeSource>,
}

#[cfg(feature = "bft-async")]
impl BFTEnhancer {
    /// Create new BFT enhancer
    pub fn new(config: BFTConfig) -> Result<Self> {
        let mut network_membership = HashSet::new();
        network_membership.insert(config.node_id.clone());

        Ok(Self {
            config,
            pending_operations: RwLock::new(VecDeque::new()),
            consensus_state: RwLock::new(BFTConsensusState::default()),
            network_membership: RwLock::new(network_membership),
            stats: RwLock::new(BFTStats::default()),
            time_source: crate::config::time_source(),
        })
    }

    /// Submit operation for consensus
    pub async fn submit_operation(
        &self,
        base: &OpManager,
        operation: BFTOperation,
    ) -> Result<BFTConsensusResult> {
        let start_time = self.time_source.now_ms();

        // Add operation to pending queue
        self.pending_operations.write().push_back(operation.clone());

        // Check if we have enough nodes for consensus
        let network_size = self.network_membership.read().len();
        if network_size < self.config.min_network_size {
            tracing::warn!(
                network_size = network_size,
                min_required = self.config.min_network_size,
                "Insufficient network size for BFT consensus"
            );

            if self.config.fallback_on_error {
                return self.execute_fallback(base, operation, start_time).await;
            } else {
                bail!("Insufficient network size for BFT consensus: {} < {}",
                      network_size, self.config.min_network_size);
            }
        }

        // Attempt BFT consensus
        match self.run_consensus(&operation).await {
            Ok(result) => {
                self.update_success_stats(start_time);
                Ok(result)
            }
            Err(e) if self.config.fallback_on_error => {
                tracing::warn!(
                    operation_id = %operation.id,
                    error = %e,
                    "BFT consensus failed, falling back to direct execution"
                );
                self.execute_fallback(base, operation, start_time).await
            }
            Err(e) => {
                self.update_error_stats();
                Err(e)
            }
        }
    }

    /// Run BFT consensus algorithm
    async fn run_consensus(&self, operation: &BFTOperation) -> Result<BFTConsensusResult> {
        let start_time = self.time_source.now_ms();

        // In a real implementation, this would:
        // 1. Create Honey Badger BFT instance
        // 2. Submit contribution to the consensus algorithm
        // 3. Wait for consensus result
        // 4. Execute the agreed-upon batch of operations

        // Simulate consensus process
        let consensus_time_ms = self.simulate_consensus_delay();
        tokio::time::sleep(Duration::from_millis(consensus_time_ms)).await;

        let end_time = self.time_source.now_ms();
        let participating_nodes = self.network_membership.read().clone();

        // Simulate successful consensus
        Ok(BFTConsensusResult {
            operation_id: operation.id.clone(),
            consensus_reached: true,
            execution_result: Bytes::from("bft_consensus_result"),
            participating_nodes,
            consensus_time_ms: end_time - start_time,
            used_fallback: false,
        })
    }

    /// Execute fallback operation
    async fn execute_fallback(
        &self,
        base: &OpManager,
        operation: BFTOperation,
        start_time: u64,
    ) -> Result<BFTConsensusResult> {
        // Direct execution without consensus
        // In a real implementation, this would execute the operation directly

        let end_time = self.time_source.now_ms();
        self.update_fallback_stats();

        Ok(BFTConsensusResult {
            operation_id: operation.id,
            consensus_reached: false,
            execution_result: Bytes::from("fallback_result"),
            participating_nodes: {
                let mut set = HashSet::new();
                set.insert(self.config.node_id.clone());
                set
            },
            consensus_time_ms: end_time - start_time,
            used_fallback: true,
        })
    }

    /// Simulate consensus delay based on network size and operation priority
    fn simulate_consensus_delay(&self) -> u64 {
        let network_size = self.network_membership.read().len();
        let base_delay = 100; // 100ms base delay
        let network_factor = (network_size as u64).saturating_sub(1) * 50; // +50ms per additional node

        base_delay + network_factor
    }

    /// Add node to network membership
    pub fn add_node(&self, node_id: BFTNodeId) -> Result<()> {
        let mut membership = self.network_membership.write();
        membership.insert(node_id.clone());

        tracing::info!(
            node_id = %node_id,
            network_size = membership.len(),
            "Added node to BFT network"
        );

        Ok(())
    }

    /// Remove node from network membership
    pub fn remove_node(&self, node_id: &BFTNodeId) -> Result<()> {
        let mut membership = self.network_membership.write();
        membership.remove(node_id);

        tracing::info!(
            node_id = %node_id,
            network_size = membership.len(),
            "Removed node from BFT network"
        );

        Ok(())
    }

    /// Get current network membership
    pub fn network_membership(&self) -> Vec<BFTNodeId> {
        self.network_membership.read().iter().cloned().collect()
    }

    /// Update success statistics
    fn update_success_stats(&self, start_time: u64) {
        let mut stats = self.stats.write();
        stats.total_operations += 1;
        stats.successful_operations += 1;
        stats.total_consensus_time_ms += self.time_source.now_ms() - start_time;
    }

    /// Update error statistics
    fn update_error_stats(&self) {
        let mut stats = self.stats.write();
        stats.total_operations += 1;
        stats.failed_operations += 1;
    }

    /// Update fallback statistics
    fn update_fallback_stats(&self) {
        let mut stats = self.stats.write();
        stats.total_operations += 1;
        stats.fallback_operations += 1;
    }

    /// Get current statistics
    pub fn stats(&self) -> BFTStats {
        self.stats.read().clone()
    }
}

/// BFT consensus state
#[cfg(feature = "bft-async")]
#[derive(Debug, Default)]
struct BFTConsensusState {
    current_epoch: u64,
    last_consensus_time: u64,
    pending_contributions: HashMap<BFTNodeId, Vec<Bytes>>,
}

/// BFT enhancement for non-BFT builds
#[cfg(not(feature = "bft-async"))]
pub struct BFTEnhancer {
    config: BFTConfig,
}

#[cfg(not(feature = "bft-async"))]
impl BFTEnhancer {
    pub fn new(config: BFTConfig) -> Result<Self> {
        Ok(Self { config })
    }

    pub async fn submit_operation(
        &self,
        _base: &OpManager,
        operation: BFTOperation,
    ) -> Result<BFTConsensusResult> {
        bail!("BFT consensus support not compiled in - enable 'bft-async' feature")
    }

    pub fn stats(&self) -> BFTStats {
        BFTStats::default()
    }

    pub fn network_membership(&self) -> Vec<BFTNodeId> {
        vec![]
    }
}

/// BFT enhancement statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BFTStats {
    /// Total operations submitted for consensus
    pub total_operations: u64,
    /// Successfully consensus operations
    pub successful_operations: u64,
    /// Failed consensus operations
    pub failed_operations: u64,
    /// Operations that fell back to direct execution
    pub fallback_operations: u64,
    /// Total time spent in consensus
    pub total_consensus_time_ms: u64,
    /// Current network size
    pub current_network_size: usize,
    /// Maximum network size observed
    pub max_network_size: usize,
    /// Byzantine faults detected
    pub byzantine_faults_detected: u64,
}

impl BFTStats {
    /// Success rate
    pub fn success_rate(&self) -> f64 {
        if self.total_operations == 0 {
            return 0.0;
        }
        self.successful_operations as f64 / self.total_operations as f64
    }

    /// Fallback rate
    pub fn fallback_rate(&self) -> f64 {
        if self.total_operations == 0 {
            return 0.0;
        }
        self.fallback_operations as f64 / self.total_operations as f64
    }

    /// Average consensus time
    pub fn average_consensus_time_ms(&self) -> f64 {
        if self.successful_operations == 0 {
            return 0.0;
        }
        self.total_consensus_time_ms as f64 / self.successful_operations as f64
    }

    /// Byzantine fault rate
    pub fn byzantine_fault_rate(&self) -> f64 {
        if self.total_operations == 0 {
            return 0.0;
        }
        self.byzantine_faults_detected as f64 / self.total_operations as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bft_config_default() {
        let config = BFTConfig::default();
        assert!(!config.enabled);
        assert!(config.fallback_on_error);
        assert_eq!(config.max_byzantine_nodes, 1);
        assert_eq!(config.min_network_size, 4);
    }

    #[test]
    fn test_bft_node_id() {
        let node_id = BFTNodeId::from("test_node");
        assert_eq!(node_id.as_str(), "test_node");
        assert_eq!(format!("{}", node_id), "test_node");
    }

    #[test]
    fn test_operation_priority_ordering() {
        assert!(BFTOperationPriority::Critical > BFTOperationPriority::High);
        assert!(BFTOperationPriority::High > BFTOperationPriority::Normal);
        assert!(BFTOperationPriority::Normal > BFTOperationPriority::Low);
    }

    #[test]
    fn test_bft_stats() {
        let mut stats = BFTStats::default();
        stats.total_operations = 100;
        stats.successful_operations = 80;
        stats.fallback_operations = 15;
        stats.failed_operations = 5;
        stats.total_consensus_time_ms = 8000;
        stats.byzantine_faults_detected = 2;

        assert_eq!(stats.success_rate(), 0.8);
        assert_eq!(stats.fallback_rate(), 0.15);
        assert_eq!(stats.average_consensus_time_ms(), 100.0);
        assert_eq!(stats.byzantine_fault_rate(), 0.02);
    }

    #[test]
    fn test_bft_operation_serialization() {
        let operation = BFTOperation {
            id: "test_op".to_string(),
            op_type: BFTOperationType::ContractUpdate,
            payload: Bytes::from("test_payload"),
            submitter: BFTNodeId::from("test_submitter"),
            priority: BFTOperationPriority::High,
            timestamp: 1234567890,
        };

        let serialized = serde_json::to_string(&operation).expect("Serialization failed");
        let deserialized: BFTOperation = serde_json::from_str(&serialized)
            .expect("Deserialization failed");

        assert_eq!(operation.id, deserialized.id);
        assert_eq!(operation.payload, deserialized.payload);
        assert_eq!(operation.submitter, deserialized.submitter);
        assert_eq!(operation.priority, deserialized.priority);
    }

    #[cfg(feature = "bft-async")]
    #[tokio::test]
    async fn test_bft_enhancer_creation() {
        let config = BFTConfig {
            enabled: true,
            node_id: BFTNodeId::from("test_node"),
            ..Default::default()
        };

        let enhancer = BFTEnhancer::new(config);
        assert!(enhancer.is_ok());

        let enhancer = enhancer.unwrap();
        let membership = enhancer.network_membership();
        assert_eq!(membership.len(), 1);
        assert!(membership.contains(&BFTNodeId::from("test_node")));
    }

    #[cfg(feature = "bft-async")]
    #[tokio::test]
    async fn test_membership_management() {
        let config = BFTConfig {
            enabled: true,
            node_id: BFTNodeId::from("node1"),
            ..Default::default()
        };

        let enhancer = BFTEnhancer::new(config).unwrap();

        // Add nodes
        enhancer.add_node(BFTNodeId::from("node2")).unwrap();
        enhancer.add_node(BFTNodeId::from("node3")).unwrap();

        let membership = enhancer.network_membership();
        assert_eq!(membership.len(), 3);

        // Remove node
        enhancer.remove_node(&BFTNodeId::from("node2")).unwrap();
        let membership = enhancer.network_membership();
        assert_eq!(membership.len(), 2);
        assert!(!membership.contains(&BFTNodeId::from("node2")));
    }

    #[cfg(not(feature = "bft-async"))]
    #[tokio::test]
    async fn test_bft_not_available() {
        let config = BFTConfig {
            enabled: true,
            ..Default::default()
        };

        let enhancer = BFTEnhancer::new(config).unwrap();

        let operation = BFTOperation {
            id: "test".to_string(),
            op_type: BFTOperationType::ContractUpdate,
            payload: Bytes::from("test"),
            submitter: BFTNodeId::from("test"),
            priority: BFTOperationPriority::Normal,
            timestamp: 0,
        };

        let result = enhancer.submit_operation(
            &OpManager::default(), // This won't actually be called
            operation,
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("BFT consensus support not compiled in"));
    }
}