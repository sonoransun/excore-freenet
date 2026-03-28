//! Authentication provider trait and error types.

use super::{ClientIdentity, Credential};

/// Result of an authentication attempt.
pub type AuthResult = Result<ClientIdentity, AuthError>;

/// Errors that can occur during authentication.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AuthError {
    #[error("no credentials provided")]
    NoCredentials,
    #[error("invalid credentials: {reason}")]
    InvalidCredentials { reason: String },
    #[error("expired credentials")]
    Expired,
    #[error("authentication provider unavailable: {reason}")]
    ProviderUnavailable { reason: String },
}

/// Trait implemented by authentication providers.
///
/// Each provider validates a specific credential type (API key, bearer token,
/// etc.) and returns a `ClientIdentity` on success. Providers are composable:
/// the `AuthChain` tries each provider in order until one succeeds or all
/// reject the credential.
pub trait AuthProvider: Send + Sync {
    /// Returns `true` if this provider can handle the given credential type.
    fn supports(&self, credential: &Credential) -> bool;

    /// Attempt to authenticate the given credential.
    ///
    /// Returns `Ok(identity)` on success, `Err(AuthError)` on failure.
    /// Should return `Err(AuthError::NoCredentials)` if the credential type
    /// is not supported (callers typically check `supports()` first).
    fn authenticate(&self, credential: &Credential) -> AuthResult;
}

/// Tries a list of providers in order; returns the first successful identity.
pub struct AuthChain {
    providers: Vec<Box<dyn AuthProvider>>,
    /// When `true`, unauthenticated requests are allowed (backward compat).
    permissive: bool,
}

impl AuthChain {
    pub fn new(providers: Vec<Box<dyn AuthProvider>>, permissive: bool) -> Self {
        Self {
            providers,
            permissive,
        }
    }

    /// Authenticate a credential against the chain.
    ///
    /// In permissive mode, `Credential::None` yields an anonymous identity
    /// for the given `client_id` instead of an error.
    pub fn authenticate(
        &self,
        credential: &Credential,
        client_id: crate::client_events::ClientId,
    ) -> AuthResult {
        if matches!(credential, Credential::None) {
            return if self.permissive {
                Ok(ClientIdentity::Anonymous(client_id))
            } else {
                Err(AuthError::NoCredentials)
            };
        }

        for provider in &self.providers {
            if provider.supports(credential) {
                match provider.authenticate(credential) {
                    Ok(identity) => return Ok(identity),
                    Err(AuthError::NoCredentials) => continue,
                    Err(e) => return Err(e),
                }
            }
        }

        if self.permissive {
            Ok(ClientIdentity::Anonymous(client_id))
        } else {
            Err(AuthError::InvalidCredentials {
                reason: "no provider accepted the credential".into(),
            })
        }
    }
}

impl std::fmt::Debug for AuthChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthChain")
            .field("providers", &self.providers.len())
            .field("permissive", &self.permissive)
            .finish()
    }
}
