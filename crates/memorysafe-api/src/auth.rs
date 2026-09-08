use crate::AppState;
use crate::error::ApiError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use memorysafe_auth::Authenticated;
use memorysafe_core::{Actor, TenantId};

/// A handler that takes this parameter cannot have skipped authentication —
/// there is no other way to construct one.
pub struct Auth(pub Authenticated);

impl Auth {
    pub fn tenant(&self) -> &TenantId {
        self.0.tenant()
    }

    pub fn subject(&self) -> &memorysafe_core::SubjectId {
        self.0.subject()
    }

    pub fn actor(&self) -> Actor {
        self.0.actor()
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        Ok(Auth(state.resolver.authenticate(&parts.headers)?))
    }
}
