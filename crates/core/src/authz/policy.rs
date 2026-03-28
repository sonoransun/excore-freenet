//! Roles, permissions, and access policy definitions.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Operations that can be performed on resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// Read contract state or delegate data.
    Read,
    /// Write (PUT) contract state.
    Write,
    /// Execute delegate operations.
    Execute,
    /// Subscribe to contract state changes.
    Subscribe,
    /// Publish state updates (UPDATE operation).
    Publish,
    /// Administrative operations (node management, key management).
    Admin,
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
            Self::Execute => write!(f, "execute"),
            Self::Subscribe => write!(f, "subscribe"),
            Self::Publish => write!(f, "publish"),
            Self::Admin => write!(f, "admin"),
        }
    }
}

/// Categories of resources that can be protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceType {
    /// Contract operations (GET, PUT, UPDATE, SUBSCRIBE).
    Contract,
    /// Delegate operations.
    Delegate,
    /// Network/node management operations.
    Network,
    /// Administrative endpoints (key management, config, etc.).
    Admin,
}

impl std::fmt::Display for ResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contract => write!(f, "contract"),
            Self::Delegate => write!(f, "delegate"),
            Self::Network => write!(f, "network"),
            Self::Admin => write!(f, "admin"),
        }
    }
}

/// Built-in roles with pre-defined permission sets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    /// Full access to all operations.
    Admin,
    /// Standard user: read, write, execute, subscribe, publish on contracts
    /// and delegates. No admin access.
    User,
    /// Read-only: can GET contracts and subscribe, but cannot PUT/UPDATE.
    ReadOnly,
    /// Custom role with a name. Permissions are looked up in the policy.
    Custom(String),
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admin => write!(f, "admin"),
            Self::User => write!(f, "user"),
            Self::ReadOnly => write!(f, "read-only"),
            Self::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

/// Maps a role to its set of (resource_type, permission) grants.
#[derive(Debug, Clone)]
pub struct RolePermissions {
    grants: HashSet<(ResourceType, Permission)>,
}

impl RolePermissions {
    fn new(grants: impl IntoIterator<Item = (ResourceType, Permission)>) -> Self {
        Self {
            grants: grants.into_iter().collect(),
        }
    }

    /// Returns `true` if this role grants the given permission on the resource.
    pub fn allows(&self, resource: ResourceType, permission: Permission) -> bool {
        self.grants.contains(&(resource, permission))
    }
}

/// Default permission sets for built-in roles.
fn default_admin_permissions() -> RolePermissions {
    use Permission::*;
    use ResourceType::*;
    let all_perms = [Read, Write, Execute, Subscribe, Publish, Permission::Admin];
    let all_resources = [Contract, Delegate, Network, ResourceType::Admin];
    RolePermissions::new(
        all_resources
            .iter()
            .flat_map(|r| all_perms.iter().map(move |p| (*r, *p))),
    )
}

fn default_user_permissions() -> RolePermissions {
    use Permission::*;
    use ResourceType::*;
    RolePermissions::new([
        (Contract, Read),
        (Contract, Write),
        (Contract, Subscribe),
        (Contract, Publish),
        (Delegate, Read),
        (Delegate, Execute),
        (Network, Read),
    ])
}

fn default_readonly_permissions() -> RolePermissions {
    use Permission::*;
    use ResourceType::*;
    RolePermissions::new([
        (Contract, Read),
        (Contract, Subscribe),
        (Delegate, Read),
        (Network, Read),
    ])
}

/// Central access policy that resolves roles to permissions.
///
/// Thread-safe and cheaply cloneable (wraps an `Arc`). Custom roles can be
/// registered at runtime; built-in roles always resolve to their defaults.
#[derive(Debug, Clone)]
pub struct AccessPolicy {
    custom_roles: Arc<RwLock<HashMap<String, RolePermissions>>>,
}

impl Default for AccessPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl AccessPolicy {
    pub fn new() -> Self {
        Self {
            custom_roles: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Resolve a role to its permission set.
    pub fn permissions_for(&self, role: &Role) -> RolePermissions {
        match role {
            Role::Admin => default_admin_permissions(),
            Role::User => default_user_permissions(),
            Role::ReadOnly => default_readonly_permissions(),
            Role::Custom(name) => self.custom_roles.read().get(name).cloned().unwrap_or_else(
                || {
                    tracing::warn!(role = %name, "Unknown custom role, defaulting to read-only");
                    default_readonly_permissions()
                },
            ),
        }
    }

    /// Register a custom role with a specific set of permissions.
    pub fn register_custom_role(
        &self,
        name: String,
        grants: impl IntoIterator<Item = (ResourceType, Permission)>,
    ) {
        self.custom_roles
            .write()
            .insert(name, RolePermissions::new(grants));
    }

    /// Check if any of the given roles grant the specified permission on the resource.
    pub fn check(&self, roles: &[Role], resource: ResourceType, permission: Permission) -> bool {
        roles
            .iter()
            .any(|role| self.permissions_for(role).allows(resource, permission))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_admin_has_all_permissions() {
        let policy = AccessPolicy::new();
        assert!(policy.check(&[Role::Admin], ResourceType::Contract, Permission::Write));
        assert!(policy.check(&[Role::Admin], ResourceType::Admin, Permission::Admin));
        assert!(policy.check(&[Role::Admin], ResourceType::Network, Permission::Read));
    }

    #[test]
    fn test_user_cannot_admin() {
        let policy = AccessPolicy::new();
        assert!(!policy.check(&[Role::User], ResourceType::Admin, Permission::Admin));
        assert!(policy.check(&[Role::User], ResourceType::Contract, Permission::Write));
    }

    #[test]
    fn test_readonly_cannot_write() {
        let policy = AccessPolicy::new();
        assert!(!policy.check(&[Role::ReadOnly], ResourceType::Contract, Permission::Write));
        assert!(policy.check(&[Role::ReadOnly], ResourceType::Contract, Permission::Read));
        assert!(policy.check(
            &[Role::ReadOnly],
            ResourceType::Contract,
            Permission::Subscribe
        ));
    }

    #[test]
    fn test_custom_role() {
        let policy = AccessPolicy::new();
        policy.register_custom_role(
            "publisher".into(),
            [
                (ResourceType::Contract, Permission::Read),
                (ResourceType::Contract, Permission::Publish),
            ],
        );

        assert!(policy.check(
            &[Role::Custom("publisher".into())],
            ResourceType::Contract,
            Permission::Publish
        ));
        assert!(!policy.check(
            &[Role::Custom("publisher".into())],
            ResourceType::Contract,
            Permission::Write
        ));
    }

    #[test]
    fn test_unknown_custom_role_defaults_to_readonly() {
        let policy = AccessPolicy::new();
        // Unknown custom role should default to read-only
        assert!(policy.check(
            &[Role::Custom("nonexistent".into())],
            ResourceType::Contract,
            Permission::Read
        ));
        assert!(!policy.check(
            &[Role::Custom("nonexistent".into())],
            ResourceType::Contract,
            Permission::Write
        ));
    }

    #[test]
    fn test_multiple_roles_union() {
        let policy = AccessPolicy::new();
        // ReadOnly + custom publisher should get union of both
        policy.register_custom_role(
            "publisher".into(),
            [(ResourceType::Contract, Permission::Publish)],
        );

        let roles = [Role::ReadOnly, Role::Custom("publisher".into())];
        assert!(policy.check(&roles, ResourceType::Contract, Permission::Read));
        assert!(policy.check(&roles, ResourceType::Contract, Permission::Publish));
        assert!(!policy.check(&roles, ResourceType::Contract, Permission::Write));
    }
}
