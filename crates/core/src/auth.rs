//! Authentication module for Freenet Core.
//!
//! Provides extensible authentication providers that validate client credentials
//! and produce a `SecurityContext` for use in authorization decisions.

mod api_key;
#[cfg(feature = "websocket")]
pub mod middleware;
mod provider;

pub use api_key::{ApiKey, ApiKeyProvider, ApiKeyStore};
pub use provider::{AuthError, AuthProvider, AuthResult};

use serde::{Deserialize, Serialize};

use crate::client_events::{AuthToken, ClientId};

/// Identifies an authenticated client.
///
/// In permissive mode (backward compatibility), clients receive an anonymous
/// identity tied to their `ClientId`. Authenticated clients carry a named
/// principal derived from their credential.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClientIdentity {
    /// Unauthenticated client (backward-compatible default).
    Anonymous(ClientId),
    /// Authenticated via API key.
    ApiKey { key_id: String, name: String },
    /// Authenticated via bearer token / external identity provider.
    Bearer { subject: String, issuer: String },
}

impl ClientIdentity {
    /// Returns `true` if this is an anonymous (unauthenticated) identity.
    pub fn is_anonymous(&self) -> bool {
        matches!(self, Self::Anonymous(_))
    }

    /// Returns a short display label for logging.
    pub fn label(&self) -> String {
        match self {
            Self::Anonymous(id) => format!("anon:{id}"),
            Self::ApiKey { name, .. } => format!("apikey:{name}"),
            Self::Bearer { subject, .. } => format!("bearer:{subject}"),
        }
    }
}

impl std::fmt::Display for ClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

/// Credential extracted from a client request for authentication.
#[derive(Debug, Clone)]
pub enum Credential {
    /// Legacy `AuthToken` from existing Freenet protocol.
    LegacyToken(AuthToken),
    /// API key (typically passed in an `X-API-Key` header or query param).
    ApiKey(String),
    /// Bearer token (JWT / opaque token from an external IdP).
    BearerToken(String),
    /// No credential presented.
    None,
}
