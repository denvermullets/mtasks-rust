//! The Release manifest: the single public `latest.json` describing the latest
//! Release and carrying the Release notes of every Release (schema 1).
//!
//! Pure: bytes in, typed value out. Any malformed input is a
//! [`ManifestError`], which is an ordinary check failure, never a security
//! failure.

use chrono::{DateTime, FixedOffset};
use semver::Version;
use serde_json::Value;

/// The highest manifest schema this app understands.
pub const KNOWN_SCHEMA: u64 = 1;

/// A manifest that can't be used. Always an ordinary check failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is not valid JSON: {0}")]
    BadJson(String),
    #[error("manifest is missing the required field `{0}`")]
    MissingField(&'static str),
    #[error("manifest field `{0}` is invalid: {1}")]
    InvalidField(&'static str, String),
    #[error("unsupported manifest schema {0}")]
    UnsupportedSchema(u64),
}

/// One Release's entry in the cumulative `releases` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseEntry {
    pub version: Version,
    pub published_at: DateTime<FixedOffset>,
    /// Markdown.
    pub notes: String,
}

/// Release notes, or the fact that they can't be read. Unavailable notes never
/// block an Update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseNotes {
    /// Manifest order as parsed; newest first after [`ReleaseNotes::between`].
    Available(Vec<ReleaseEntry>),
    Unavailable,
}

/// A schema 1 manifest: an Update that the app can install itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: Version,
    pub published_at: DateTime<FixedOffset>,
    pub zip_url: String,
    /// 64 lowercase hex characters.
    pub zip_sha256: String,
    pub zip_size: u64,
    pub dmg_url: String,
    pub releases: ReleaseNotes,
}

/// A manifest with a schema newer than this app knows: an Update exists but
/// can't self-install, so the DMG is offered instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmgOffer {
    pub version: Version,
    pub dmg_url: String,
    pub releases: ReleaseNotes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedManifest {
    SelfInstall(Manifest),
    DmgOnly(DmgOffer),
}

/// Parse the bytes of `latest.json`.
pub fn parse(bytes: &[u8]) -> Result<ParsedManifest, ManifestError> {
    let root: Value =
        serde_json::from_slice(bytes).map_err(|e| ManifestError::BadJson(e.to_string()))?;
    let obj = root
        .as_object()
        .ok_or_else(|| ManifestError::BadJson("top level is not an object".into()))?;

    let schema = match obj.get("schema") {
        None => return Err(ManifestError::MissingField("schema")),
        Some(v) => v
            .as_u64()
            .ok_or_else(|| ManifestError::InvalidField("schema", "not an integer".into()))?,
    };
    if schema == 0 {
        return Err(ManifestError::UnsupportedSchema(schema));
    }

    let version = parse_version(obj.get("version"), "version")?;
    let releases = parse_releases(obj.get("releases"));

    if schema > KNOWN_SCHEMA {
        let dmg_url = req_url(obj.get("dmg_url"), "dmg_url")?;
        return Ok(ParsedManifest::DmgOnly(DmgOffer {
            version,
            dmg_url,
            releases,
        }));
    }

    let published_at = parse_time(obj.get("published_at"), "published_at")?;
    let zip_url = req_url(obj.get("zip_url"), "zip_url")?;
    let zip_sha256 = req_str(obj.get("zip_sha256"), "zip_sha256")?;
    if !is_sha256_hex(&zip_sha256) {
        return Err(ManifestError::InvalidField(
            "zip_sha256",
            "not 64 lowercase hex characters".into(),
        ));
    }
    let zip_size = match obj.get("zip_size") {
        None => return Err(ManifestError::MissingField("zip_size")),
        Some(v) => v.as_u64().ok_or_else(|| {
            ManifestError::InvalidField("zip_size", "not a non-negative integer".into())
        })?,
    };
    let dmg_url = req_url(obj.get("dmg_url"), "dmg_url")?;

    Ok(ParsedManifest::SelfInstall(Manifest {
        version,
        published_at,
        zip_url,
        zip_sha256,
        zip_size,
        dmg_url,
        releases,
    }))
}

/// The only host a manifest may point downloads at.
pub const RELEASES_HOST: &str = "github.com";
/// Path prefix of the public releases-only repo's download URLs.
pub const RELEASES_PATH_PREFIX: &str = "/denvermullets/jait-releases/releases/download/";

/// Whether `url` is an absolute https URL on the releases host. The app only
/// ever uses URLs that pass this.
pub fn is_releases_url(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    u.scheme() == "https"
        && u.host_str() == Some(RELEASES_HOST)
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none()
        && u.path().starts_with(RELEASES_PATH_PREFIX)
}

fn req_url(v: Option<&Value>, field: &'static str) -> Result<String, ManifestError> {
    let s = req_str(v, field)?;
    if is_releases_url(&s) {
        Ok(s)
    } else {
        Err(ManifestError::InvalidField(
            field,
            "not an https URL on the releases host".into(),
        ))
    }
}

fn req_str(v: Option<&Value>, field: &'static str) -> Result<String, ManifestError> {
    match v {
        None => Err(ManifestError::MissingField(field)),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(ManifestError::InvalidField(field, "not a string".into())),
    }
}

fn parse_version(v: Option<&Value>, field: &'static str) -> Result<Version, ManifestError> {
    let s = req_str(v, field)?;
    Version::parse(&s).map_err(|e| ManifestError::InvalidField(field, e.to_string()))
}

fn parse_time(
    v: Option<&Value>,
    field: &'static str,
) -> Result<DateTime<FixedOffset>, ManifestError> {
    let s = req_str(v, field)?;
    DateTime::parse_from_rfc3339(&s).map_err(|e| ManifestError::InvalidField(field, e.to_string()))
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A missing or broken `releases` (not an array, or any item unreadable) means
/// "Release notes unavailable". It never fails the parse.
fn parse_releases(v: Option<&Value>) -> ReleaseNotes {
    let Some(Value::Array(items)) = v else {
        return ReleaseNotes::Unavailable;
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(o) = item.as_object() else {
            return ReleaseNotes::Unavailable;
        };
        let (Ok(version), Ok(published_at), Ok(notes)) = (
            parse_version(o.get("version"), "version"),
            parse_time(o.get("published_at"), "published_at"),
            req_str(o.get("notes"), "notes"),
        ) else {
            return ReleaseNotes::Unavailable;
        };
        out.push(ReleaseEntry {
            version,
            published_at,
            notes,
        });
    }
    ReleaseNotes::Available(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn valid() -> Value {
        serde_json::json!({
            "schema": 1,
            "version": "0.13.0",
            "published_at": "2026-11-17T10:00:00Z",
            "zip_url": "https://github.com/denvermullets/jait-releases/releases/download/v0.13.0/JAIT-0.13.0-arm64.zip",
            "zip_sha256": HASH,
            "zip_size": 48213504u64,
            "dmg_url": "https://github.com/denvermullets/jait-releases/releases/download/v0.13.0/JAIT-0.13.0.dmg",
            "releases": [
                { "version": "0.13.0", "published_at": "2026-11-17T10:00:00Z", "notes": "- a" }
            ]
        })
    }

    fn parse_value(v: &Value) -> Result<ParsedManifest, ManifestError> {
        parse(v.to_string().as_bytes())
    }

    fn without(field: &str) -> Value {
        let mut v = valid();
        v.as_object_mut().unwrap().remove(field);
        v
    }

    fn with(field: &str, val: Value) -> Value {
        let mut v = valid();
        v[field] = val;
        v
    }

    #[test]
    fn valid_schema_1_parses() {
        let ParsedManifest::SelfInstall(m) = parse_value(&valid()).unwrap() else {
            panic!("expected self-install");
        };
        assert_eq!(m.version, Version::new(0, 13, 0));
        assert_eq!(m.zip_sha256, HASH);
        assert_eq!(m.zip_size, 48213504);
        let ReleaseNotes::Available(r) = m.releases else {
            panic!("notes should be available");
        };
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].notes, "- a");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut v = valid();
        v["future"] = serde_json::json!({"x": 1});
        v["releases"][0]["extra"] = serde_json::json!(true);
        assert!(matches!(
            parse_value(&v),
            Ok(ParsedManifest::SelfInstall(_))
        ));
    }

    #[test]
    fn bad_json_is_a_check_failure() {
        assert!(matches!(parse(b"{nope"), Err(ManifestError::BadJson(_))));
        assert!(matches!(parse(b"[]"), Err(ManifestError::BadJson(_))));
    }

    #[test]
    fn missing_required_fields_fail() {
        for f in [
            "schema",
            "version",
            "published_at",
            "zip_url",
            "zip_sha256",
            "zip_size",
            "dmg_url",
        ] {
            assert!(parse_value(&without(f)).is_err(), "missing {f} should fail");
        }
    }

    #[test]
    fn bad_version_fails() {
        for bad in ["v0.13.0", "0.13", "latest", ""] {
            assert!(
                matches!(
                    parse_value(&with("version", bad.into())),
                    Err(ManifestError::InvalidField("version", _))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn bad_hash_fails() {
        let upper = HASH.to_uppercase();
        let long = format!("{HASH}0");
        for bad in ["abc", upper.as_str(), &HASH[..63], long.as_str()] {
            assert!(
                matches!(
                    parse_value(&with("zip_sha256", bad.into())),
                    Err(ManifestError::InvalidField("zip_sha256", _))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn https_url_on_the_releases_host_is_accepted() {
        assert!(is_releases_url(
            "https://github.com/denvermullets/jait-releases/releases/download/v1.0.0/JAIT.dmg"
        ));
    }

    #[test]
    fn other_urls_are_check_failures() {
        let base = "github.com/denvermullets/jait-releases/releases/download/v1/x.zip";
        let bad = [
            format!("http://{base}"),
            "/releases/download/v1/x.zip".to_string(),
            "x.zip".to_string(),
            "https://evil.example/denvermullets/jait-releases/releases/download/v1/x.zip"
                .to_string(),
            "https://github.com.evil.example/denvermullets/jait-releases/releases/download/v1/x.zip"
                .to_string(),
            "https://github.com/other/repo/releases/download/v1/x.zip".to_string(),
            format!("https://user@{base}"),
            format!("https://github.com:8443/{}", &base[11..]),
            "file:///tmp/x.zip".to_string(),
            "".to_string(),
        ];
        for url in bad {
            for field in ["zip_url", "dmg_url"] {
                assert!(
                    matches!(
                        parse_value(&with(field, url.clone().into())),
                        Err(ManifestError::InvalidField(f, _)) if f == field
                    ),
                    "{field} {url:?}"
                );
            }
        }
    }

    #[test]
    fn missing_or_broken_releases_does_not_fail_the_parse() {
        let broken = [
            without("releases"),
            with("releases", "nope".into()),
            with("releases", serde_json::json!([{ "version": "x" }])),
            with("releases", serde_json::json!([1])),
        ];
        for v in broken {
            let ParsedManifest::SelfInstall(m) = parse_value(&v).unwrap() else {
                panic!("expected self-install");
            };
            assert_eq!(m.releases, ReleaseNotes::Unavailable);
        }
    }
}
