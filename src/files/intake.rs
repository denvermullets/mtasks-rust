//! Intake: turn picked files into Pending uploads. Validate each file, copy it
//! to the outbox, record the pending row and queue its upload. Shared by the
//! file picker, drag-and-drop and the attach shortcut.

use std::path::PathBuf;

use rusqlite::Connection;

use crate::db::attachments::{self, Attachment, NewPending, OwnerKind, UploadKind};
use crate::files::outbox;
use crate::files::validate::{content_type_for_filename, validate_upload, Validation};

/// What happened to a batch of picked files.
#[derive(Debug, Default)]
pub struct Intake {
    /// New `pending` rows, in pick order.
    pub queued: Vec<Attachment>,
    /// Toast-ready reasons for the files that were not queued.
    pub refused: Vec<String>,
}

impl Intake {
    /// One toast body listing every refused file with its reason (None when
    /// nothing was refused).
    pub fn refusal_toast(&self) -> Option<String> {
        (!self.refused.is_empty()).then(|| self.refused.join("\n"))
    }
}

/// The file's name and content type when it may be attached, else a
/// toast-ready reason. Reads only its metadata.
fn check_file(path: &std::path::Path) -> Result<(String, Option<&'static str>), String> {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let size = std::fs::metadata(path)
        .map_err(|e| format!("{filename}: can't read file ({e})."))?
        .len();
    let content_type = content_type_for_filename(&filename);
    if let Validation::Refused(reason) =
        validate_upload(&filename, size, content_type, UploadKind::Attachment)
    {
        return Err(reason);
    }
    Ok((filename, content_type))
}

/// Split picked files into the ones that may be attached and toast-ready
/// reasons for the rest, without staging anything. The new-issue form holds
/// files this way until the issue exists to own them.
pub fn screen_files(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<String>) {
    let mut ok = Vec::new();
    let mut refused = Vec::new();
    for path in paths {
        match check_file(&path) {
            Ok(_) => ok.push(path),
            Err(reason) => refused.push(reason),
        }
    }
    (ok, refused)
}

/// Queue each picked file as an Attachment of `owner`. A bad file is refused
/// (reason in [`Intake::refused`]) without blocking the rest.
pub fn queue_files(
    conn: &Connection,
    outbox_root: &std::path::Path,
    owner_kind: OwnerKind,
    owner_id: &str,
    paths: &[PathBuf],
) -> Intake {
    let mut out = Intake::default();
    for path in paths {
        let (filename, content_type) = match check_file(path) {
            Ok(ok) => ok,
            Err(reason) => {
                out.refused.push(reason);
                continue;
            }
        };
        let staged = match outbox::stage(outbox_root, path) {
            Ok(s) => s,
            Err(e) => {
                out.refused.push(format!("{filename}: can't copy file ({e})."));
                continue;
            }
        };
        let new = NewPending {
            owner_kind,
            owner_id: owner_id.to_string(),
            filename: filename.clone(),
            content_type: content_type.unwrap_or("application/octet-stream").to_string(),
            byte_size: staged.byte_size as i64,
            checksum: staged.checksum.clone(),
            outbox_path: staged.path.to_string_lossy().into_owned(),
            kind: UploadKind::Attachment,
            placeholder_uuid: None,
        };
        match attachments::add_pending(conn, &new) {
            Ok(a) => out.queued.push(a),
            Err(e) => {
                let _ = outbox::remove(&staged.path);
                out.refused.push(format!("{filename}: {e}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("intake-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&c).unwrap();
        c
    }

    #[test]
    fn good_files_are_queued_and_bad_ones_refused_without_blocking() {
        let d = dir();
        let good = d.join("notes.txt");
        let empty = d.join("empty.txt");
        let svg = d.join("logo.svg");
        std::fs::write(&good, b"hello").unwrap();
        std::fs::write(&empty, b"").unwrap();
        std::fs::write(&svg, b"<svg/>").unwrap();
        let c = conn();

        let r = queue_files(
            &c,
            &d.join("outbox"),
            OwnerKind::Issue,
            "i1",
            &[svg, good.clone(), empty],
        );

        assert_eq!(r.refused.len(), 2);
        assert_eq!(r.queued.len(), 1);
        let a = &r.queued[0];
        assert_eq!((a.filename.as_str(), a.content_type.as_str()), ("notes.txt", "text/plain"));
        assert_eq!(a.byte_size, 5);
        // The outbox copy outlives the source.
        std::fs::remove_file(&good).unwrap();
        assert_eq!(std::fs::read(a.outbox_path.as_ref().unwrap()).unwrap(), b"hello");
        // Upload + attach queued for the one good file only.
        assert_eq!(crate::db::sync_queue::pending_count(&c).unwrap(), 2);
    }

    #[test]
    fn screening_keeps_good_files_and_stages_nothing() {
        let d = dir();
        let good = d.join("notes.txt");
        let empty = d.join("empty.txt");
        std::fs::write(&good, b"hello").unwrap();
        std::fs::write(&empty, b"").unwrap();

        let (ok, refused) = screen_files(vec![empty, good.clone(), d.join("missing.txt")]);

        assert_eq!(ok, vec![good]);
        assert_eq!(refused.len(), 2);
        assert!(!d.join("outbox").exists());
    }

    #[test]
    fn all_bad_batch_queues_nothing_and_one_toast_lists_each_file() {
        let d = dir();
        let empty = d.join("empty.txt");
        let svg = d.join("logo.svg");
        std::fs::write(&empty, b"").unwrap();
        std::fs::write(&svg, b"<svg/>").unwrap();
        let c = conn();
        let r = queue_files(&c, &d.join("outbox"), OwnerKind::Issue, "i1", &[empty, svg]);
        assert!(r.queued.is_empty());
        assert_eq!(crate::db::sync_queue::pending_count(&c).unwrap(), 0);
        let toast = r.refusal_toast().unwrap();
        assert!(toast.contains("empty.txt") && toast.contains("logo.svg"));
        assert!(Intake::default().refusal_toast().is_none());
    }

    #[test]
    fn unreadable_file_is_refused() {
        let d = dir();
        let c = conn();
        let r = queue_files(&c, &d.join("outbox"), OwnerKind::Issue, "i1", &[d.join("gone.txt")]);
        assert!(r.queued.is_empty());
        assert_eq!(r.refused.len(), 1);
    }
}
