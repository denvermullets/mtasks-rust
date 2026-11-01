//! Upload checks. Constants mirror the Rails Attachment validations.

/// Maximum upload size: 25 MB.
pub const MAX_BYTE_SIZE: u64 = 25 * 1024 * 1024;

pub const ALLOWED_CONTENT_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/heic",
    "image/avif",
    "application/pdf",
    "text/plain",
    "text/markdown",
    "text/csv",
    "application/json",
    "application/zip",
    "application/msword",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.ms-excel",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.ms-powerpoint",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "video/mp4",
    "video/quicktime",
    "video/webm",
];

/// What the file is being uploaded as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadKind {
    Attachment,
    InlineImage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validation {
    Ok,
    /// Human-readable reason, suitable for a toast.
    Refused(String),
}

impl Validation {
    pub fn is_ok(&self) -> bool {
        matches!(self, Validation::Ok)
    }
}

/// Content type guessed from the file extension (case-insensitive).
/// Returns `None` for unknown extensions.
pub fn content_type_for_filename(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "html" | "htm" => "text/html",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        _ => return None,
    })
}

/// Check a file before upload. `content_type` is normally
/// `content_type_for_filename(filename)`; pass `None` when the extension is unknown.
pub fn validate_upload(
    filename: &str,
    byte_size: u64,
    content_type: Option<&str>,
    kind: UploadKind,
) -> Validation {
    if byte_size == 0 {
        return Validation::Refused(format!("{filename} is empty."));
    }
    if byte_size > MAX_BYTE_SIZE {
        return Validation::Refused(format!(
            "{filename} is too large (limit is {} MB).",
            MAX_BYTE_SIZE / (1024 * 1024)
        ));
    }
    let Some(ct) = content_type else {
        return Validation::Refused(format!("{filename}: unknown file type."));
    };
    if !ALLOWED_CONTENT_TYPES.contains(&ct) {
        return Validation::Refused(format!("{filename}: {ct} files are not allowed."));
    }
    if kind == UploadKind::InlineImage && !ct.starts_with("image/") {
        return Validation::Refused(format!("{filename} is not an image."));
    }
    Validation::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB25: u64 = 25 * 1024 * 1024;

    fn check(name: &str, size: u64, kind: UploadKind) -> Validation {
        validate_upload(name, size, content_type_for_filename(name), kind)
    }

    #[test]
    fn size_cap_edges() {
        let a = UploadKind::Attachment;
        assert!(!check("a.png", 0, a).is_ok());
        assert!(check("a.png", 1, a).is_ok());
        assert!(check("a.png", MB25, a).is_ok());
        assert!(!check("a.png", MB25 + 1, a).is_ok());
    }

    #[test]
    fn refusal_reasons_are_readable() {
        let Validation::Refused(r) = check("big.png", MB25 + 1, UploadKind::Attachment) else {
            panic!()
        };
        assert_eq!(r, "big.png is too large (limit is 25 MB).");
        let Validation::Refused(r) = check("e.png", 0, UploadKind::Attachment) else {
            panic!()
        };
        assert_eq!(r, "e.png is empty.");
    }

    #[test]
    fn allowed_types() {
        for n in [
            "a.png", "a.JPG", "a.jpeg", "a.gif", "a.webp", "a.heic", "a.avif", "a.pdf", "a.txt",
            "a.md", "a.csv", "a.json", "a.zip", "a.doc", "a.docx", "a.xls", "a.xlsx", "a.ppt",
            "a.pptx", "a.mp4", "a.mov", "a.webm",
        ] {
            assert!(check(n, 10, UploadKind::Attachment).is_ok(), "{n}");
        }
    }

    #[test]
    fn svg_and_html_refused() {
        for n in ["a.svg", "a.html", "a.htm"] {
            assert!(!check(n, 10, UploadKind::Attachment).is_ok(), "{n}");
        }
    }

    #[test]
    fn unknown_extension_refused() {
        assert!(!check("a.exe", 10, UploadKind::Attachment).is_ok());
        assert!(!check("noext", 10, UploadKind::Attachment).is_ok());
    }

    #[test]
    fn inline_images_must_be_images() {
        assert!(check("a.png", 10, UploadKind::InlineImage).is_ok());
        assert!(!check("a.pdf", 10, UploadKind::InlineImage).is_ok());
        assert!(check("a.pdf", 10, UploadKind::Attachment).is_ok());
    }
}
