//! Pushing the Home page choice.

use crate::api::ApiClient;
use crate::sync::engine::open;
use super::outcome::{Outcome, classify};

/// `PUT /me/home_page`. A 422 drops the entry (logged by the drain); the next
/// `/me` read restores the server value.
pub(super) async fn home_page_set(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let resolved = match open() {
        Ok(conn) => crate::db::home_page::resolve_payload(&conn, payload),
        Err(e) => return Outcome::LeavePending(e),
    };
    let (kind, id) = match resolved {
        crate::db::home_page::Resolved::Ready(k, i) => (k, i),
        crate::db::home_page::Resolved::Waiting => {
            return Outcome::LeavePending("Home view or dashboard not synced yet".into())
        }
        crate::db::home_page::Resolved::Bad => {
            return Outcome::DropAndLog("home_page set: bad payload".into())
        }
    };
    match crate::api::users::set_home_page(client, &kind, id).await {
        Ok(()) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::api::ApiError;
    use crate::db::sync_queue;
    use crate::sync::push::fixtures::{saved_view_conn, saved_view_dto};
    use crate::sync::push::saved_view::commit_saved_view_create;

    #[test]
    fn home_page_422_is_dropped_and_logged_then_me_restores() {
        use crate::db::home_page;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        home_page::set_local(&conn, "team", 3).unwrap();
        let entry = sync_queue::pending_entries(&conn).unwrap().remove(0);
        // The drain's view of the entry: payload parses, a 422 classifies as a drop.
        assert_eq!(home_page::parse_payload(entry.payload.as_deref()), Some(("team".into(), 3)));
        let e = ApiError::Validation { errors: vec!["not found".into()] };
        assert!(matches!(classify(&e), Outcome::DropAndLog(_)));
        sync_queue::mark_synced(&conn, entry.id.unwrap()).unwrap();
        home_page::apply_me(&conn, Some(&crate::api::HomePageDto { kind: "team".into(), id: 1 })).unwrap();
        assert_eq!(home_page::get(&conn).unwrap().unwrap().target, home_page::HomeTarget::Team(1));
    }

    #[test]
    fn a_view_created_offline_then_set_as_home_waits_then_resolves_after_its_create() {
        use crate::db::home_page::{self, Resolved};
        let (mut conn, id) = saved_view_conn();
        home_page::set_view_local(&conn, &id, None).unwrap();
        let pending = sync_queue::pending_entries(&conn).unwrap();
        // FIFO: the create drains before the home write.
        assert_eq!((pending[0].entity_type.as_str(), pending[1].entity_type.as_str()), ("saved_view", "home_page"));
        assert_eq!(home_page::resolve_payload(&conn, pending[1].payload.as_deref()), Resolved::Waiting);
        // The create pushes; the queued PUT now carries the view's remote id.
        commit_saved_view_create(&mut conn, pending[0].id.unwrap(), &id, &saved_view_dto(88)).unwrap();
        assert_eq!(
            home_page::resolve_payload(&conn, pending[1].payload.as_deref()),
            Resolved::Ready("view".into(), 88)
        );
        assert_eq!(home_page::get(&conn).unwrap().unwrap().target, home_page::HomeTarget::View(88));
    }

    #[test]
    fn a_5xx_leaves_the_home_page_write_pending() {
        let e = ApiError::Server { status: 500, message: "x".into() };
        assert!(matches!(classify(&e), Outcome::LeavePending(_)));
    }
}
