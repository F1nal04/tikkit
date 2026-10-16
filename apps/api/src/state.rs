use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    auth::{AuthRuntime, AuthSettings},
    config::Config,
};

/// Shared state handed to every handler.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    /// Password hasher, session policy and login/setup rate limiters.
    pub auth: Arc<AuthRuntime>,
}

impl AppState {
    /// State with the production authentication policy.
    pub fn new(db: PgPool, config: Config) -> Self {
        Self {
            db,
            config: Arc::new(config),
            auth: Arc::new(AuthRuntime::default()),
        }
    }

    /// Replaces the authentication settings (tests use
    /// [`AuthSettings::insecure_fast_for_tests`]).
    pub fn with_auth_settings(mut self, settings: AuthSettings) -> Self {
        self.auth = Arc::new(AuthRuntime::new(settings));
        self
    }
}
