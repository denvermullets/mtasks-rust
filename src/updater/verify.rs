//! Verify and stage a downloaded Update (JRST-516).
//!
//! Steps, in order: unzip into `staged-<ver>/`, `codesign` against a
//! requirement pinned to the running app's own Team ID, `spctl`, check the
//! bundle's version, write the `verified` marker. The decisions are pure; the
//! `Tools` trait is the only thing that spawns processes.

use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;

use super::download;
use super::staging::VERIFIED_MARKER;

pub const BUNDLE_ID: &str = "com.jait.desktop";
pub const APP_NAME: &str = "JAIT.app";

/// How staging failed: the updater treats the two differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageError {
    /// Unzip failed or the archive is not what it should be.
    Broken(String),
    /// codesign, spctl or the version check refused the bundle.
    Security(String),
}

/// What one stage run needs.
#[derive(Debug, Clone)]
pub struct StageRequest {
    pub version: Version,
    pub staging_dir: PathBuf,
    /// The running bundle, whose signature supplies the Team ID.
    pub running_bundle: PathBuf,
    pub running_version: Version,
}

/// `staged-<ver>`: the folder name of a Staged update.
pub fn staged_dir_name(version: &Version) -> String {
    format!("staged-{version}")
}

pub fn staged_dir(staging_dir: &Path, version: &Version) -> PathBuf {
    staging_dir.join(staged_dir_name(version))
}

/// The codesign requirement a new bundle must satisfy. `team` must already be
/// validated by [`parse_team_id`].
pub fn requirement(team: &str) -> String {
    format!(
        "anchor apple generic and identifier \"{BUNDLE_ID}\" and certificate leaf[subject.OU] = \"{team}\""
    )
}

/// The Team ID from `codesign -dvv` output (`TeamIdentifier=ABCDE12345`).
/// `None` for ad-hoc/unsigned (`not set`) or anything not shaped like a Team ID.
pub fn parse_team_id(codesign_output: &str) -> Option<String> {
    let value = codesign_output
        .lines()
        .find_map(|l| l.trim().strip_prefix("TeamIdentifier="))?
        .trim();
    let valid = value.len() == 10 && value.chars().all(|c| c.is_ascii_alphanumeric());
    valid.then(|| value.to_string())
}

/// The bundle's version must equal the manifest's and be newer than running.
pub fn check_version(found: &str, expected: &Version, running: &Version) -> Result<(), String> {
    let found = Version::parse(found.trim())
        .map_err(|_| format!("the update's version \"{}\" is not valid", found.trim()))?;
    if &found != expected {
        return Err(format!(
            "the update says it is {found}, but the release manifest promised {expected}"
        ));
    }
    if &found <= running {
        return Err(format!("the update ({found}) is not newer than this app ({running})"));
    }
    Ok(())
}

/// The process edge. `Err` is the tool's own message.
pub trait Tools {
    /// `ditto -x -k <zip> <dest>`
    fn unzip(&self, zip: &Path, dest: &Path) -> Result<(), String>;
    /// `codesign -dvv <bundle>`, combined output.
    fn describe_signature(&self, bundle: &Path) -> Result<String, String>;
    /// `codesign --verify --deep --strict -R=<requirement> <bundle>`
    fn codesign_verify(&self, bundle: &Path, requirement: &str) -> Result<(), String>;
    /// `spctl --assess --type execute <bundle>`
    fn spctl_assess(&self, bundle: &Path) -> Result<(), String>;
    /// `CFBundleShortVersionString` of the bundle.
    fn bundle_version(&self, bundle: &Path) -> Result<String, String>;
}

/// Run the whole stage. On success the zip is deleted and `staged-<ver>/`
/// holds `JAIT.app` plus the verified marker. On failure the staged dir is
/// removed (the zip is left for the machine's `DeleteStaging`).
pub fn stage(tools: &dyn Tools, req: &StageRequest) -> Result<(), StageError> {
    let zip = download::zip_path(&req.staging_dir, &req.version);
    let dest = staged_dir(&req.staging_dir, &req.version);
    let result = run_steps(tools, req, &zip, &dest);
    match &result {
        Ok(()) => {
            let _ = fs::remove_file(&zip);
        }
        Err(_) => {
            let _ = fs::remove_dir_all(&dest);
        }
    }
    result
}

fn run_steps(
    tools: &dyn Tools,
    req: &StageRequest,
    zip: &Path,
    dest: &Path,
) -> Result<(), StageError> {
    let broken = |what: &str, e: String| StageError::Broken(format!("{what}: {e}"));
    let insecure = |what: &str, e: String| StageError::Security(format!("{what}: {e}"));

    let _ = fs::remove_dir_all(dest);
    fs::create_dir_all(dest).map_err(|e| broken("could not prepare the update folder", e.to_string()))?;
    tools
        .unzip(zip, dest)
        .map_err(|e| broken("the update could not be unzipped", e))?;
    let app = dest.join(APP_NAME);
    if !app.is_dir() {
        return Err(StageError::Broken(format!("the update does not contain {APP_NAME}")));
    }

    let sig = tools
        .describe_signature(&req.running_bundle)
        .map_err(|e| insecure("could not read this app's signature", e))?;
    let team = parse_team_id(&sig).ok_or_else(|| {
        StageError::Security("this app has no Team ID, so an update can't be trusted".into())
    })?;
    tools
        .codesign_verify(&app, &requirement(&team))
        .map_err(|e| insecure("the update's signature was refused", e))?;
    tools
        .spctl_assess(&app)
        .map_err(|e| insecure("Gatekeeper refused the update", e))?;
    let found = tools
        .bundle_version(&app)
        .map_err(|e| insecure("could not read the update's version", e))?;
    check_version(&found, &req.version, &req.running_version).map_err(StageError::Security)?;

    fs::write(dest.join(VERIFIED_MARKER), req.version.to_string())
        .map_err(|e| broken("could not mark the update verified", e.to_string()))
}

/// Remove every trace of one version: zip, partial, staged dir. Best effort.
pub fn delete_version(staging_dir: &Path, version: &Version) {
    let _ = fs::remove_file(download::zip_path(staging_dir, version));
    let _ = fs::remove_file(download::part_path(staging_dir, version));
    let _ = fs::remove_dir_all(staged_dir(staging_dir, version));
}

/// Of the verified `staged-<ver>` dirs, the newest that beats `running`, plus
/// the names of the rest (older or not newer) to delete. Pure.
pub fn pick_staged(verified_dir_names: &[String], running: &Version) -> (Option<Version>, Vec<String>) {
    let mut best: Option<Version> = None;
    let mut stale = Vec::new();
    for name in verified_dir_names {
        let v = name.strip_prefix("staged-").and_then(|s| Version::parse(s).ok());
        match v {
            Some(v) if &v > running && best.as_ref().is_none_or(|b| &v > b) => {
                if let Some(old) = best.replace(v) {
                    stale.push(staged_dir_name(&old));
                }
            }
            _ => stale.push(name.clone()),
        }
    }
    (best, stale)
}

/// Launch: find the verified Staged update left by a past run, deleting the
/// others. Returns the version to restore. Missing folder = none.
pub fn restore_staged(staging_dir: &Path, running: &Version) -> Option<Version> {
    let names: Vec<String> = fs::read_dir(staging_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let p = e.path();
            p.is_dir() && p.join(VERIFIED_MARKER).is_file() && p.join(APP_NAME).is_dir()
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let (best, stale) = pick_staged(&names, running);
    for name in stale {
        let _ = fs::remove_dir_all(staging_dir.join(name));
    }
    best
}

/// Real tools: the macOS command-line utilities.
pub struct SystemTools;

fn run(cmd: &mut std::process::Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("could not run {:?}: {e}", cmd.get_program()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

impl Tools for SystemTools {
    fn unzip(&self, zip: &Path, dest: &Path) -> Result<(), String> {
        run(std::process::Command::new("ditto").args(["-x", "-k"]).arg(zip).arg(dest)).map(drop)
    }

    fn describe_signature(&self, bundle: &Path) -> Result<String, String> {
        run(std::process::Command::new("codesign").arg("-dvv").arg(bundle))
    }

    fn codesign_verify(&self, bundle: &Path, requirement: &str) -> Result<(), String> {
        run(std::process::Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(format!("-R={requirement}"))
            .arg(bundle))
        .map(drop)
    }

    fn spctl_assess(&self, bundle: &Path) -> Result<(), String> {
        run(std::process::Command::new("spctl")
            .args(["--assess", "--type", "execute"])
            .arg(bundle))
        .map(drop)
    }

    fn bundle_version(&self, bundle: &Path) -> Result<String, String> {
        run(std::process::Command::new("plutil")
            .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
            .arg(bundle.join("Contents/Info.plist")))
        .map(|s| s.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/verify").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn requirement_pins_bundle_id_and_team() {
        assert_eq!(
            requirement("ABCDE12345"),
            "anchor apple generic and identifier \"com.jait.desktop\" and certificate leaf[subject.OU] = \"ABCDE12345\""
        );
    }

    #[test]
    fn team_id_is_read_from_codesign_output() {
        let out = "Identifier=com.jait.desktop\nAuthority=Developer ID Application: X (ABCDE12345)\nTeamIdentifier=ABCDE12345\n";
        assert_eq!(parse_team_id(out).as_deref(), Some("ABCDE12345"));
    }

    #[test]
    fn ad_hoc_or_malformed_team_ids_are_rejected() {
        assert_eq!(parse_team_id("TeamIdentifier=not set\n"), None);
        assert_eq!(parse_team_id("Identifier=x\n"), None);
        assert_eq!(parse_team_id("TeamIdentifier=AB\" or 1\n"), None);
    }

    #[test]
    fn version_must_match_manifest_and_beat_running() {
        let (exp, run) = (v("0.13.0"), v("0.12.0"));
        assert!(check_version("0.13.0\n", &exp, &run).is_ok());
        assert!(check_version("0.13.1", &exp, &run).unwrap_err().contains("promised 0.13.0"));
        assert!(check_version("0.13.0", &exp, &v("0.13.0")).unwrap_err().contains("not newer"));
        assert!(check_version("nope", &exp, &run).unwrap_err().contains("not valid"));
    }

    #[test]
    fn pick_staged_takes_newest_newer_and_marks_the_rest_stale() {
        let names = ["staged-0.13.0", "staged-0.14.0", "staged-0.11.0", "junk"]
            .map(String::from)
            .to_vec();
        let (best, mut stale) = pick_staged(&names, &v("0.12.0"));
        stale.sort();
        assert_eq!(best, Some(v("0.14.0")));
        assert_eq!(stale, vec!["junk", "staged-0.11.0", "staged-0.13.0"]);
        assert_eq!(pick_staged(&names[2..3], &v("0.12.0")).0, None);
    }

    struct Fake {
        calls: RefCell<Vec<&'static str>>,
        unzip: Result<(), String>,
        make_app: bool,
        signature: Result<String, String>,
        codesign: Result<(), String>,
        spctl: Result<(), String>,
        version: Result<String, String>,
        requirement_seen: RefCell<String>,
    }

    impl Fake {
        fn good() -> Fake {
            Fake {
                calls: RefCell::new(vec![]),
                unzip: Ok(()),
                make_app: true,
                signature: Ok("TeamIdentifier=ABCDE12345".into()),
                codesign: Ok(()),
                spctl: Ok(()),
                version: Ok("0.13.0".into()),
                requirement_seen: RefCell::new(String::new()),
            }
        }
    }

    impl Tools for Fake {
        fn unzip(&self, _zip: &Path, dest: &Path) -> Result<(), String> {
            self.calls.borrow_mut().push("unzip");
            if self.make_app {
                fs::create_dir_all(dest.join(APP_NAME)).unwrap();
            }
            self.unzip.clone()
        }
        fn describe_signature(&self, _b: &Path) -> Result<String, String> {
            self.signature.clone()
        }
        fn codesign_verify(&self, _b: &Path, req: &str) -> Result<(), String> {
            self.calls.borrow_mut().push("codesign");
            *self.requirement_seen.borrow_mut() = req.to_string();
            self.codesign.clone()
        }
        fn spctl_assess(&self, _b: &Path) -> Result<(), String> {
            self.calls.borrow_mut().push("spctl");
            self.spctl.clone()
        }
        fn bundle_version(&self, _b: &Path) -> Result<String, String> {
            self.calls.borrow_mut().push("version");
            self.version.clone()
        }
    }

    fn request(name: &str) -> StageRequest {
        let staging_dir = scratch(name);
        fs::write(download::zip_path(&staging_dir, &v("0.13.0")), b"zip").unwrap();
        StageRequest {
            version: v("0.13.0"),
            staging_dir,
            running_bundle: PathBuf::from("/Applications/JAIT.app"),
            running_version: v("0.12.0"),
        }
    }

    #[test]
    fn a_genuine_newer_zip_is_staged_with_a_marker_and_the_zip_is_deleted() {
        let req = request("ok");
        let tools = Fake::good();
        assert_eq!(stage(&tools, &req), Ok(()));
        let dir = staged_dir(&req.staging_dir, &req.version);
        assert!(dir.join(VERIFIED_MARKER).is_file());
        assert!(dir.join(APP_NAME).is_dir());
        assert!(!download::zip_path(&req.staging_dir, &req.version).exists());
        assert_eq!(*tools.calls.borrow(), vec!["unzip", "codesign", "spctl", "version"]);
        // TEAM came from the running app's signature.
        assert!(tools.requirement_seen.borrow().ends_with("= \"ABCDE12345\""));
    }

    #[test]
    fn a_failed_unzip_is_a_broken_download() {
        let req = request("unzip");
        let mut tools = Fake::good();
        tools.unzip = Err("truncated".into());
        assert!(matches!(stage(&tools, &req), Err(StageError::Broken(m)) if m.contains("truncated")));
        assert!(!staged_dir(&req.staging_dir, &req.version).exists());
    }

    #[test]
    fn a_zip_without_the_app_is_a_broken_download() {
        let req = request("noapp");
        let mut tools = Fake::good();
        tools.make_app = false;
        assert!(matches!(stage(&tools, &req), Err(StageError::Broken(_))));
    }

    #[test]
    fn wrong_team_bundle_id_or_signature_is_a_security_failure_and_cleans_up() {
        let req = request("codesign");
        let mut tools = Fake::good();
        tools.codesign = Err("does not satisfy requirement".into());
        assert!(matches!(stage(&tools, &req), Err(StageError::Security(_))));
        assert!(!staged_dir(&req.staging_dir, &req.version).exists());
        assert_eq!(*tools.calls.borrow(), vec!["unzip", "codesign"]);
    }

    #[test]
    fn a_failed_spctl_is_a_security_failure() {
        let req = request("spctl");
        let mut tools = Fake::good();
        tools.spctl = Err("rejected".into());
        assert!(matches!(stage(&tools, &req), Err(StageError::Security(_))));
    }

    #[test]
    fn a_version_mismatch_is_a_security_failure_with_no_marker() {
        let req = request("version");
        let mut tools = Fake::good();
        tools.version = Ok("0.12.5".into());
        assert!(matches!(stage(&tools, &req), Err(StageError::Security(_))));
        assert!(!staged_dir(&req.staging_dir, &req.version).exists());
    }

    #[test]
    fn a_running_app_without_a_team_id_cannot_verify_anything() {
        let req = request("noteam");
        let mut tools = Fake::good();
        tools.signature = Ok("TeamIdentifier=not set".into());
        assert!(matches!(stage(&tools, &req), Err(StageError::Security(_))));
        assert!(!tools.calls.borrow().contains(&"codesign"));
    }

    #[test]
    fn restore_finds_the_verified_newer_dir_and_deletes_the_rest() {
        let dir = scratch("restore");
        for (name, marker) in [("staged-0.13.0", true), ("staged-0.11.0", true), ("staged-0.14.0", false)] {
            fs::create_dir_all(dir.join(name).join(APP_NAME)).unwrap();
            if marker {
                fs::write(dir.join(name).join(VERIFIED_MARKER), b"").unwrap();
            }
        }
        assert_eq!(restore_staged(&dir, &v("0.12.0")), Some(v("0.13.0")));
        assert!(!dir.join("staged-0.11.0").exists());
        assert!(dir.join("staged-0.13.0").exists());
        assert_eq!(restore_staged(&dir.join("missing"), &v("0.12.0")), None);
    }
}
