//! Notification poll: the driver-side hook for the Notifications cadence.
//!
//! JRST-88 ships a stub that only logs; JRST-89 replaces the body of [`poll`]
//! with the real fetch + `db::notifications::apply_poll`.

use crate::api::{ApiClient, ApiError};

/// How a poll went. The driver maps this onto `CycleFinished` / auth pause.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // error variants are produced by the real poll (JRST-89)
pub enum PollOutcome {
    Ok,
    /// 401: goes down the same auth-pause path as a Data sync 401.
    AuthFailed,
    Failed(String),
}

impl PollOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(self, PollOutcome::Ok)
    }

    pub fn auth_failed(&self) -> bool {
        matches!(self, PollOutcome::AuthFailed)
    }

    /// Classify an API error: 401 pauses both cadences, anything else backs off.
    #[allow(dead_code)] // used by the real poll (JRST-89)
    pub fn from_error(e: &ApiError) -> Self {
        match e {
            ApiError::Unauthorized(_) => PollOutcome::AuthFailed,
            other => PollOutcome::Failed(other.to_string()),
        }
    }
}

/// Run one Notification poll. STUB (JRST-88): logs and succeeds.
pub async fn poll(_client: &ApiClient) -> PollOutcome {
    eprintln!("[notif-poll] stub poll fired");
    PollOutcome::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_pauses_other_errors_back_off() {
        let auth = PollOutcome::from_error(&ApiError::Unauthorized("x".into()));
        assert!(auth.auth_failed() && !auth.succeeded());
        let other = PollOutcome::from_error(&ApiError::NotFound("x".into()));
        assert!(!other.auth_failed() && !other.succeeded());
        assert!(PollOutcome::Ok.succeeded());
    }
}
