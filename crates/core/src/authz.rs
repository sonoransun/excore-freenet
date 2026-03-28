//! Authorization module for Freenet Core.
//!
//! Implements role-based access control (RBAC) with resource-level permissions.
//! A `SecurityContext` carries the authenticated identity plus resolved roles
//! and permissions, and is threaded through request processing to gate operations.

mod context;
mod policy;

pub use context::SecurityContext;
pub use policy::{AccessPolicy, Permission, ResourceType, Role, RolePermissions};
