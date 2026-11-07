//! Pushing a sidebar group's collapsed state.

use super::outcome::{classify, Outcome};
use crate::api::ApiClient;

/// `PATCH /me/sidebar_groups/:id`. A 404 (group deleted on the web) drops the
/// entry; the next `/me` pull removes the group locally.
pub(super) async fn sidebar_group_update(
    client: &ApiClient,
    group_id: &str,
    payload: Option<&str>,
) -> Outcome {
    let Some(collapsed) = crate::db::sidebar_layout::parse_payload(payload) else {
        return Outcome::DropAndLog("sidebar_group update: bad payload".into());
    };
    match crate::api::users::set_sidebar_group_collapsed(client, group_id, collapsed).await {
        Ok(()) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiError;

    #[test]
    fn a_deleted_group_404_is_dropped() {
        let e = ApiError::NotFound("Sidebar group not found".into());
        assert!(matches!(classify(&e), Outcome::DropAndLog(_)));
    }

    #[test]
    fn a_5xx_leaves_the_collapse_pending_and_a_scope_403_pauses_pushes() {
        let e = ApiError::Server { status: 503, message: "x".into() };
        assert!(matches!(classify(&e), Outcome::LeavePending(_)));
        let e = ApiError::Forbidden("Token lacks required scope".into());
        assert!(matches!(classify(&e), Outcome::PausePushes(..)));
    }
}
