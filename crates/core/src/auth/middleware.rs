//! Axum middleware for extracting credentials and building a `SecurityContext`.
//!
//! The middleware runs early in the request pipeline and attaches a
//! `SecurityContext` as a request extension. Downstream handlers retrieve it
//! via `Extension<SecurityContext>`.
//!
//! Credential extraction order:
//! 1. `Authorization: Bearer <token>` header
//! 2. `X-API-Key` header
//! 3. Legacy `AuthToken` from query params (backward compat)
//! 4. No credential (anonymous in permissive mode)

use std::sync::Arc;

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::auth::{AuthProvider, ClientIdentity, Credential};
use crate::authz::{AccessPolicy, Role, SecurityContext};
use crate::client_events::ClientId;

use super::api_key::ApiKeyStore;
use super::provider::AuthChain;

/// Shared state for the security middleware.
///
/// Cheaply cloneable (all fields are `Arc`-wrapped). Created once at server
/// startup and installed as an axum `Extension`.
#[derive(Debug, Clone)]
pub struct SecurityState {
    auth_chain: Arc<AuthChain>,
    policy: AccessPolicy,
    api_key_store: ApiKeyStore,
}

impl SecurityState {
    /// Create a new security state with the given configuration.
    pub fn new(permissive: bool, api_key_store: ApiKeyStore) -> Self {
        let providers: Vec<Box<dyn AuthProvider>> = vec![Box::new(
            super::api_key::ApiKeyProvider::new(api_key_store.clone()),
        )];

        let auth_chain = Arc::new(AuthChain::new(providers, permissive));
        let policy = AccessPolicy::new();

        Self {
            auth_chain,
            policy,
            api_key_store,
        }
    }

    /// Create a fully permissive security state (backward-compatible default).
    pub fn permissive() -> Self {
        Self::new(true, ApiKeyStore::new())
    }

    /// Returns a reference to the API key store.
    pub fn api_key_store(&self) -> &ApiKeyStore {
        &self.api_key_store
    }

    /// Returns a reference to the access policy.
    pub fn policy(&self) -> &AccessPolicy {
        &self.policy
    }

    /// Authenticate a credential and build a security context.
    pub fn authenticate(&self, credential: &Credential, client_id: ClientId) -> SecurityContext {
        match self.auth_chain.authenticate(credential, client_id) {
            Ok(identity) => {
                let roles = self.resolve_roles(&identity);
                SecurityContext::new(identity, client_id, roles, self.policy.clone())
            }
            Err(_) => {
                // Fall back to anonymous in permissive mode (AuthChain handles this),
                // or return a context with no roles for enforcement mode.
                SecurityContext::new(
                    ClientIdentity::Anonymous(client_id),
                    client_id,
                    vec![],
                    self.policy.clone(),
                )
            }
        }
    }

    /// Resolve roles for an authenticated identity.
    fn resolve_roles(&self, identity: &ClientIdentity) -> Vec<Role> {
        match identity {
            ClientIdentity::Anonymous(_) => vec![Role::User],
            ClientIdentity::ApiKey { key_id, .. } => {
                // Look up roles from the API key store
                let keys = self.api_key_store.list_keys();
                keys.into_iter()
                    .find(|k| k.key_id == *key_id)
                    .map(|k| k.roles)
                    .unwrap_or_else(|| vec![Role::User])
            }
            ClientIdentity::Bearer { .. } => {
                // Bearer tokens default to User role; future OAuth2
                // integration will extract roles from token claims.
                vec![Role::User]
            }
        }
    }
}

/// Extract a credential from an HTTP request.
pub fn extract_credential(req: &Request) -> Credential {
    let headers = req.headers();

    // 1. Check Authorization: Bearer <token>
    if let Some(auth_header) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(value) = auth_header.to_str() {
            if let Some(token) = value.strip_prefix("Bearer ") {
                return Credential::BearerToken(token.trim().to_string());
            }
        }
    }

    // 2. Check X-API-Key header
    if let Some(api_key) = headers.get("X-API-Key") {
        if let Ok(value) = api_key.to_str() {
            return Credential::ApiKey(value.to_string());
        }
    }

    // 3. No credential found
    Credential::None
}

/// Axum middleware layer that extracts credentials, authenticates, and attaches
/// a `SecurityContext` to the request.
///
/// Install this on routes that require authorization. In permissive mode,
/// unauthenticated requests proceed with anonymous/User context.
pub async fn security_middleware(
    security_state: Option<axum::extract::Extension<SecurityState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let client_id = ClientId::next();

    let ctx = match security_state {
        Some(axum::extract::Extension(state)) => {
            let credential = extract_credential(&req);
            state.authenticate(&credential, client_id)
        }
        None => {
            // No security state configured — fully permissive
            SecurityContext::permissive(client_id)
        }
    };

    req.extensions_mut().insert(ctx);
    next.run(req).await
}

/// Middleware that requires admin access. Returns 403 if the security context
/// does not have the Admin role.
pub async fn require_admin(
    axum::extract::Extension(ctx): axum::extract::Extension<SecurityContext>,
    req: Request,
    next: Next,
) -> Response {
    if !ctx.is_admin() {
        tracing::warn!(
            identity = %ctx.identity,
            "Admin access denied"
        );
        return (StatusCode::FORBIDDEN, "Admin access required").into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::Role;

    #[test]
    fn test_permissive_security_state_allows_anonymous() {
        let state = SecurityState::permissive();
        let ctx = state.authenticate(&Credential::None, ClientId::FIRST);
        assert!(ctx.identity.is_anonymous());
        assert!(ctx.has_permission(
            crate::authz::ResourceType::Contract,
            crate::authz::Permission::Read
        ));
    }

    #[test]
    fn test_strict_security_state_denies_anonymous() {
        let state = SecurityState::new(false, ApiKeyStore::new());
        let ctx = state.authenticate(&Credential::None, ClientId::FIRST);
        // In strict mode, anonymous gets no roles
        assert!(!ctx.has_permission(
            crate::authz::ResourceType::Contract,
            crate::authz::Permission::Write
        ));
    }

    #[test]
    fn test_api_key_authentication_flow() {
        let store = ApiKeyStore::new();
        let (_key, secret) = store.create_key("test-admin".into(), vec![Role::Admin]);
        let state = SecurityState::new(false, store);

        let ctx = state.authenticate(&Credential::ApiKey(secret), ClientId::FIRST);
        assert!(ctx.is_admin());
        assert!(!ctx.identity.is_anonymous());
    }

    #[test]
    fn test_invalid_api_key_strict() {
        let state = SecurityState::new(false, ApiKeyStore::new());
        let ctx = state.authenticate(&Credential::ApiKey("bad-key".into()), ClientId::FIRST);
        // Falls back to anonymous with no roles in strict mode
        assert!(ctx.identity.is_anonymous());
        assert!(!ctx.has_permission(
            crate::authz::ResourceType::Contract,
            crate::authz::Permission::Write
        ));
    }

    #[test]
    fn test_extract_credential_bearer() {
        let req = axum::http::Request::builder()
            .header("Authorization", "Bearer my-jwt-token")
            .body(axum::body::Body::empty())
            .unwrap();
        let cred = extract_credential(&req);
        assert!(matches!(cred, Credential::BearerToken(t) if t == "my-jwt-token"));
    }

    #[test]
    fn test_extract_credential_api_key() {
        let req = axum::http::Request::builder()
            .header("X-API-Key", "my-api-key")
            .body(axum::body::Body::empty())
            .unwrap();
        let cred = extract_credential(&req);
        assert!(matches!(cred, Credential::ApiKey(k) if k == "my-api-key"));
    }

    #[test]
    fn test_extract_credential_none() {
        let req = axum::http::Request::builder()
            .body(axum::body::Body::empty())
            .unwrap();
        let cred = extract_credential(&req);
        assert!(matches!(cred, Credential::None));
    }

    #[test]
    fn test_bearer_takes_priority_over_api_key() {
        let req = axum::http::Request::builder()
            .header("Authorization", "Bearer jwt-token")
            .header("X-API-Key", "api-key")
            .body(axum::body::Body::empty())
            .unwrap();
        let cred = extract_credential(&req);
        assert!(matches!(cred, Credential::BearerToken(t) if t == "jwt-token"));
    }
}
