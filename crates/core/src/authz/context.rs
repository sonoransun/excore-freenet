//! Security context that travels with each request through the system.

use crate::auth::ClientIdentity;
use crate::client_events::ClientId;

use super::policy::{AccessPolicy, Permission, ResourceType, Role};

/// Carries the authenticated identity, resolved roles, and a reference to the
/// access policy for a single client request.
///
/// Created after authentication succeeds and threaded through operation handlers
/// so that each handler can check permissions without re-authenticating.
#[derive(Debug, Clone)]
pub struct SecurityContext {
    /// The authenticated identity of the client.
    pub identity: ClientIdentity,
    /// The underlying client ID (always present for routing responses).
    pub client_id: ClientId,
    /// Roles assigned to this client (resolved from identity + policy).
    pub roles: Vec<Role>,
    /// Access policy used for permission checks.
    policy: AccessPolicy,
}

impl SecurityContext {
    /// Create a new security context.
    pub fn new(
        identity: ClientIdentity,
        client_id: ClientId,
        roles: Vec<Role>,
        policy: AccessPolicy,
    ) -> Self {
        Self {
            identity,
            client_id,
            roles,
            policy,
        }
    }

    /// Create a permissive (backward-compatible) context for an anonymous client.
    ///
    /// In permissive mode, anonymous clients receive the `User` role, granting
    /// them the same access as pre-auth Freenet clients.
    pub fn permissive(client_id: ClientId) -> Self {
        Self {
            identity: ClientIdentity::Anonymous(client_id),
            client_id,
            roles: vec![Role::User],
            policy: AccessPolicy::new(),
        }
    }

    /// Check whether this context allows the given operation.
    pub fn has_permission(&self, resource: ResourceType, permission: Permission) -> bool {
        self.policy.check(&self.roles, resource, permission)
    }

    /// Returns `true` if this context has the `Admin` role.
    pub fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| matches!(r, Role::Admin))
    }

    /// Returns a reference to the access policy.
    pub fn policy(&self) -> &AccessPolicy {
        &self.policy
    }
}

impl std::fmt::Display for SecurityContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SecurityContext({}, roles=[{}])",
            self.identity,
            self.roles
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Error returned when a permission check fails.
#[derive(Debug, Clone, thiserror::Error)]
#[error("access denied: {identity} lacks {permission} on {resource} (roles: [{roles}])")]
pub struct AccessDenied {
    pub identity: String,
    pub resource: ResourceType,
    pub permission: Permission,
    pub roles: String,
}

impl SecurityContext {
    /// Like `has_permission` but returns a structured error on denial.
    pub fn require_permission(
        &self,
        resource: ResourceType,
        permission: Permission,
    ) -> Result<(), AccessDenied> {
        if self.has_permission(resource, permission) {
            Ok(())
        } else {
            Err(AccessDenied {
                identity: self.identity.label(),
                resource,
                permission,
                roles: self
                    .roles
                    .iter()
                    .map(|r| r.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ClientIdentity;
    use crate::authz::{Permission, ResourceType, Role};

    #[test]
    fn test_permissive_context_allows_user_operations() {
        let ctx = SecurityContext::permissive(ClientId::FIRST);
        assert!(ctx.has_permission(ResourceType::Contract, Permission::Read));
        assert!(ctx.has_permission(ResourceType::Contract, Permission::Write));
        assert!(!ctx.has_permission(ResourceType::Admin, Permission::Admin));
    }

    #[test]
    fn test_admin_context() {
        let ctx = SecurityContext::new(
            ClientIdentity::ApiKey {
                key_id: "k1".into(),
                name: "admin-key".into(),
            },
            ClientId::FIRST,
            vec![Role::Admin],
            AccessPolicy::new(),
        );
        assert!(ctx.is_admin());
        assert!(ctx.has_permission(ResourceType::Admin, Permission::Admin));
        assert!(ctx.has_permission(ResourceType::Contract, Permission::Write));
    }

    #[test]
    fn test_require_permission_denied() {
        let ctx = SecurityContext::new(
            ClientIdentity::Anonymous(ClientId::FIRST),
            ClientId::FIRST,
            vec![Role::ReadOnly],
            AccessPolicy::new(),
        );
        let result = ctx.require_permission(ResourceType::Contract, Permission::Write);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.resource, ResourceType::Contract);
        assert_eq!(err.permission, Permission::Write);
    }

    #[test]
    fn test_security_context_display() {
        let ctx = SecurityContext::permissive(ClientId::FIRST);
        let display = format!("{ctx}");
        assert!(display.contains("SecurityContext"));
        assert!(display.contains("user"));
    }
}
