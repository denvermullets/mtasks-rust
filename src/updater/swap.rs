//! Installing a Staged update: swap its bundle in for the running one.
//!
//! Decisions (which bundle, preconditions, the fallback plan) are pure and
//! tested here. `SystemOps` does the real renames. No re-verify happens: the
//! verified marker written at staging is the proof. No backup, no rollback.

use std::io;
use std::path::{Path, PathBuf};

use semver::Version;

use super::install::InstallContext;
use super::staging::VERIFIED_MARKER;
use super::verify::{staged_dir, APP_NAME};

/// What one swap touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapPlan {
    /// The installed bundle (`.../JAIT.app`).
    pub installed: PathBuf,
    /// `<staging>/staged-<ver>/`, deleted at the end (it then holds the old bundle).
    pub staged_dir: PathBuf,
    /// `<staged_dir>/JAIT.app`.
    pub staged_bundle: PathBuf,
    /// `<staged_dir>/verified`.
    pub marker: PathBuf,
    /// Where the fallback parks the old bundle: inside `staged_dir`, same volume.
    pub aside: PathBuf,
}

impl SwapPlan {
    pub fn new(installed: &Path, staging_dir: &Path, version: &Version) -> Self {
        let staged_dir = staged_dir(staging_dir, version);
        SwapPlan {
            installed: installed.to_path_buf(),
            staged_bundle: staged_dir.join(APP_NAME),
            marker: staged_dir.join(VERIFIED_MARKER),
            aside: staged_dir.join("JAIT.old.app"),
            staged_dir,
        }
    }
}

/// What a quit should install: (bundle, staging dir, version) for the ready
/// version, or nothing when the app can't self-install or no Update is staged.
pub fn quit_plan(
    install: &InstallContext,
    ready: Option<&Version>,
) -> Option<(PathBuf, PathBuf, Version)> {
    match (install, ready) {
        (InstallContext::SelfInstall { bundle, staging_dir }, Some(v)) => {
            Some((bundle.clone(), staging_dir.clone(), v.clone()))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapError {
    /// The staged bundle or its verified marker is missing; nothing was touched.
    NotReady(String),
    /// Both swap strategies failed; the installed bundle is unchanged.
    Failed(String),
}

impl std::fmt::Display for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwapError::NotReady(s) | SwapError::Failed(s) => f.write_str(s),
        }
    }
}

/// Pure precondition: both the staged bundle and its marker must exist.
pub fn preconditions(bundle_exists: bool, marker_exists: bool) -> Result<(), SwapError> {
    match (bundle_exists, marker_exists) {
        (true, true) => Ok(()),
        (false, _) => Err(SwapError::NotReady("the Staged update's bundle is missing".into())),
        (true, false) => Err(SwapError::NotReady("the Staged update was never verified".into())),
    }
}

/// The filesystem steps a swap needs.
pub trait SwapOps {
    fn exists(&self, p: &Path) -> bool;
    /// Atomically exchange two paths (`renamex_np(RENAME_SWAP)`).
    fn exchange(&self, a: &Path, b: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Bump the bundle's mtime so LaunchServices re-reads it.
    fn touch(&self, p: &Path);
    fn remove_dir_all(&self, p: &Path);
}

/// Swap the Staged update in. Atomic exchange first; on failure two renames
/// (old aside, new in), restoring the old bundle if the second fails. Then
/// touch the new bundle and delete the old one (best effort).
pub fn run(ops: &dyn SwapOps, plan: &SwapPlan) -> Result<(), SwapError> {
    preconditions(ops.exists(&plan.staged_bundle), ops.exists(&plan.marker))?;
    if ops.exchange(&plan.installed, &plan.staged_bundle).is_err() {
        two_renames(ops, plan)?;
    }
    ops.touch(&plan.installed);
    // After either path the old bundle sits inside staged_dir.
    ops.remove_dir_all(&plan.staged_dir);
    Ok(())
}

fn two_renames(ops: &dyn SwapOps, plan: &SwapPlan) -> Result<(), SwapError> {
    ops.rename(&plan.installed, &plan.aside)
        .map_err(|e| SwapError::Failed(format!("could not move the old app aside: {e}")))?;
    if let Err(e) = ops.rename(&plan.staged_bundle, &plan.installed) {
        let restored = ops.rename(&plan.aside, &plan.installed);
        return Err(SwapError::Failed(match restored {
            Ok(()) => format!("could not move the new app in: {e}"),
            Err(r) => format!(
                "could not move the new app in ({e}) and could not restore the old one ({r})"
            ),
        }));
    }
    Ok(())
}

/// The real filesystem.
pub struct SystemOps;

impl SwapOps for SystemOps {
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }

    fn exchange(&self, a: &Path, b: &Path) -> io::Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(io::Error::other);
        let (a, b) = (c(a)?, c(b)?);
        // SAFETY: both are valid NUL-terminated paths that outlive the call.
        let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn touch(&self, p: &Path) {
        let _ = std::process::Command::new("/usr/bin/touch").arg(p).status();
    }

    fn remove_dir_all(&self, p: &Path) {
        let _ = std::fs::remove_dir_all(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn plan() -> SwapPlan {
        SwapPlan::new(Path::new("/Apps/JAIT.app"), Path::new("/Apps/.jait-update"), &v("0.13.0"))
    }

    #[test]
    fn plan_points_at_the_staged_bundle_and_marker() {
        let p = plan();
        assert_eq!(p.staged_bundle, Path::new("/Apps/.jait-update/staged-0.13.0/JAIT.app"));
        assert_eq!(p.marker, Path::new("/Apps/.jait-update/staged-0.13.0/verified"));
        assert!(p.aside.starts_with(&p.staged_dir));
    }

    #[test]
    fn quit_installs_only_a_self_install_with_a_ready_update() {
        let si = InstallContext::SelfInstall {
            bundle: "/Apps/JAIT.app".into(),
            staging_dir: "/Apps/.jait-update".into(),
        };
        let ver = v("0.13.0");
        assert_eq!(
            quit_plan(&si, Some(&ver)),
            Some(("/Apps/JAIT.app".into(), "/Apps/.jait-update".into(), ver.clone()))
        );
        assert_eq!(quit_plan(&si, None), None);
        assert_eq!(quit_plan(&InstallContext::Disabled, Some(&ver)), None);
    }

    #[test]
    fn preconditions_need_bundle_and_marker() {
        assert!(preconditions(true, true).is_ok());
        assert!(matches!(preconditions(false, true), Err(SwapError::NotReady(_))));
        assert!(matches!(preconditions(true, false), Err(SwapError::NotReady(_))));
    }

    /// Records calls; `fail` names the steps that error.
    struct Fake {
        marker: bool,
        fail: Vec<&'static str>,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(fail: &[&'static str]) -> Self {
            Fake { marker: true, fail: fail.to_vec(), calls: RefCell::new(vec![]) }
        }
        fn step(&self, key: &str) -> io::Result<()> {
            self.calls.borrow_mut().push(key.to_string());
            if self.fail.contains(&key) {
                Err(io::Error::other("boom"))
            } else {
                Ok(())
            }
        }
    }

    impl SwapOps for Fake {
        fn exists(&self, p: &Path) -> bool {
            !p.ends_with("verified") || self.marker
        }
        fn exchange(&self, _: &Path, _: &Path) -> io::Result<()> {
            self.step("exchange")
        }
        fn rename(&self, from: &Path, _to: &Path) -> io::Result<()> {
            let key = if from.ends_with("JAIT.old.app") {
                "restore"
            } else if from.starts_with("/Apps/.jait-update") {
                "in"
            } else {
                "aside"
            };
            self.step(key)
        }
        fn touch(&self, _: &Path) {
            let _ = self.step("touch");
        }
        fn remove_dir_all(&self, _: &Path) {
            let _ = self.step("remove");
        }
    }

    fn calls(f: &Fake) -> Vec<String> {
        f.calls.borrow().clone()
    }

    #[test]
    fn atomic_exchange_then_touch_then_delete() {
        let f = Fake::new(&[]);
        run(&f, &plan()).unwrap();
        assert_eq!(calls(&f), ["exchange", "touch", "remove"]);
    }

    #[test]
    fn missing_marker_touches_nothing() {
        let mut f = Fake::new(&[]);
        f.marker = false;
        assert!(matches!(run(&f, &plan()), Err(SwapError::NotReady(_))));
        assert!(calls(&f).is_empty());
    }

    #[test]
    fn exchange_failure_falls_back_to_two_renames() {
        let f = Fake::new(&["exchange"]);
        run(&f, &plan()).unwrap();
        assert_eq!(calls(&f), ["exchange", "aside", "in", "touch", "remove"]);
    }

    #[test]
    fn failed_second_rename_restores_the_old_bundle() {
        let f = Fake::new(&["exchange", "in"]);
        assert!(matches!(run(&f, &plan()), Err(SwapError::Failed(_))));
        assert_eq!(calls(&f), ["exchange", "aside", "in", "restore"]);
    }

    #[test]
    fn failed_first_rename_changes_nothing() {
        let f = Fake::new(&["exchange", "aside"]);
        assert!(matches!(run(&f, &plan()), Err(SwapError::Failed(_))));
        assert_eq!(calls(&f), ["exchange", "aside"]);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/swap").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture(name: &str) -> SwapPlan {
        let root = scratch(name);
        let installed = root.join("JAIT.app");
        fs::create_dir_all(&installed).unwrap();
        fs::write(installed.join("ver"), "old").unwrap();
        let p = SwapPlan::new(&installed, &root.join(".jait-update"), &v("0.13.0"));
        fs::create_dir_all(&p.staged_bundle).unwrap();
        fs::write(p.staged_bundle.join("ver"), "new").unwrap();
        fs::write(&p.marker, b"").unwrap();
        p
    }

    #[test]
    fn real_swap_installs_the_new_bundle_and_deletes_the_old() {
        let p = fixture("real");
        run(&SystemOps, &p).unwrap();
        assert_eq!(fs::read_to_string(p.installed.join("ver")).unwrap(), "new");
        assert!(!p.staged_dir.exists());
    }

    #[test]
    fn real_swap_without_marker_leaves_the_install_alone() {
        let p = fixture("nomarker");
        fs::remove_file(&p.marker).unwrap();
        assert!(run(&SystemOps, &p).is_err());
        assert_eq!(fs::read_to_string(p.installed.join("ver")).unwrap(), "old");
        assert!(p.staged_bundle.exists());
    }

    #[test]
    fn real_fallback_path_also_swaps() {
        let p = fixture("fallback");
        two_renames(&SystemOps, &p).unwrap();
        assert_eq!(fs::read_to_string(p.installed.join("ver")).unwrap(), "new");
        assert_eq!(fs::read_to_string(p.aside.join("ver")).unwrap(), "old");
    }
}
