//! Contract lifecycle management: garbage collection, deletion, and storage quota enforcement.
//!
//! This module addresses unbounded storage growth (contract_store.rs:21 TODO) by providing:
//! - Time-bounded GC policies with TTL, usage-based, and size-based eviction
//! - Explicit contract deletion API
//! - Storage quota enforcement
//!
//! All GC exemptions are time-bounded per AGENTS.md rules: no permanent exemptions allowed.

pub(crate) mod deletion;
pub(crate) mod gc;

pub(crate) use deletion::ContractDeletion;
pub(crate) use gc::{GcConfig, GcPolicy, GcSweepResult};
