//! Consensus Infrastructure for Freenet Core
//!
//! This module provides advanced consensus capabilities including:
//! - Threshold cryptography and multi-party computation
//! - Asynchronous Byzantine fault tolerance
//! - Privacy-preserving consensus mechanisms

/// Threshold cryptography and MPC for decentralized consensus
#[cfg(feature = "threshold-consensus")]
pub mod threshold;

/// Asynchronous Byzantine Fault Tolerance consensus
#[cfg(feature = "bft-async")]
pub mod bft;

// Re-export key types for easier access
#[cfg(feature = "threshold-consensus")]
pub use threshold::{
    ThresholdConsensus, ThresholdEnhancer, ThresholdConfig,
    ThresholdOperation, ThresholdStats, ParticipantRole
};

#[cfg(feature = "bft-async")]
pub use bft::{
    BFTConsensus, BFTEnhancer, BFTConfig,
    BFTOperation, BFTStats
};