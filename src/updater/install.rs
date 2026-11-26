//! Where the app runs from, and whether it can replace itself there.
//! The decision is pure; only `InstallProbe` touches the system.

use std::path::{Path, PathBuf};

/// Name of the hidden staging folder created beside the bundle. Used only
/// when the user cache folder is on another volume than the bundle (the swap
/// needs a same-volume rename), and swept from older installs otherwise.
pub const STAGING_DIR_NAME: &str = ".jait-update";

/// Subfolder of the app's cache folder that holds Staged updates.
pub const CACHE_STAGING_NAME: &str = "update";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallContext {
    /// No enclosing `.app` (e.g. `cargo run`): the updater does nothing.
    Disabled,
    /// Can't self-install; offer the DMG instead.
    DmgOnly(DmgReason),
    /// Self-install possible; staging lives on the bundle's volume: in the
    /// app's cache folder when it shares that volume, else beside the bundle.
    SelfInstall { bundle: PathBuf, staging_dir: PathBuf },
}

impl InstallContext {
    /// Where Staged updates live, when this install can self-install.
    pub fn staging_dir(&self) -> Option<&Path> {
        match self {
            InstallContext::SelfInstall { staging_dir, .. } => Some(staging_dir),
            _ => None,
        }
    }

    /// The running `.app`, when this install can self-install.
    pub fn bundle(&self) -> Option<&Path> {
        match self {
            InstallContext::SelfInstall { bundle, .. } => Some(bundle),
            _ => None,
        }
    }

    /// A beside-the-bundle staging folder left by older versions, when this
    /// install now stages elsewhere. Remove it at launch.
    pub fn legacy_staging_dir(&self) -> Option<PathBuf> {
        let InstallContext::SelfInstall { bundle, staging_dir } = self else {
            return None;
        };
        let legacy = bundle.parent()?.join(STAGING_DIR_NAME);
        (legacy != *staging_dir).then_some(legacy)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmgReason {
    Translocated,
    FolderNotWritable,
}

/// The system edge: the only filesystem/process probes install detection needs.
pub trait InstallProbe {
    fn current_exe(&self) -> Option<PathBuf>;
    fn is_dir_writable(&self, dir: &Path) -> bool;
    /// The app's own cache folder (`~/Library/Caches/<app>`), if known.
    fn cache_dir(&self) -> Option<PathBuf>;
    /// Whether `a` and `b` (or their nearest existing ancestors) are on one volume.
    fn same_volume(&self, a: &Path, b: &Path) -> bool;
}

/// Nearest ancestor of `exe` whose name ends in `.app`.
pub fn find_app_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// macOS Gatekeeper path randomization runs quarantined apps from here.
pub fn is_translocated(bundle: &Path) -> bool {
    bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
}

pub fn detect(probe: &dyn InstallProbe) -> InstallContext {
    let Some(bundle) = probe.current_exe().and_then(|e| find_app_bundle(&e)) else {
        return InstallContext::Disabled;
    };
    if is_translocated(&bundle) {
        return InstallContext::DmgOnly(DmgReason::Translocated);
    }
    let Some(parent) = bundle.parent() else {
        return InstallContext::DmgOnly(DmgReason::FolderNotWritable);
    };
    if !probe.is_dir_writable(parent) {
        return InstallContext::DmgOnly(DmgReason::FolderNotWritable);
    }
    let staging_dir = match probe.cache_dir() {
        Some(cache) if probe.same_volume(&cache, parent) => cache.join(CACHE_STAGING_NAME),
        _ => parent.join(STAGING_DIR_NAME),
    };
    InstallContext::SelfInstall { bundle, staging_dir }
}

/// Real probe: `std::env::current_exe` and a create/remove write test.
pub struct SystemProbe;

impl InstallProbe for SystemProbe {
    fn current_exe(&self) -> Option<PathBuf> {
        // Resolve symlinks so a link into a bundle still finds the `.app`.
        std::env::current_exe().ok().map(|p| p.canonicalize().unwrap_or(p))
    }

    fn is_dir_writable(&self, dir: &Path) -> bool {
        let probe = dir.join(format!(".jait-write-test-{}", std::process::id()));
        match std::fs::File::create(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    }

    fn cache_dir(&self) -> Option<PathBuf> {
        dirs::cache_dir().map(|d| d.join(crate::config::APP_DIR))
    }

    fn same_volume(&self, a: &Path, b: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        // The cache folder may not exist yet; its nearest existing ancestor
        // is where it will be created.
        let dev = |p: &Path| p.ancestors().find_map(|a| std::fs::metadata(a).ok()).map(|m| m.dev());
        matches!((dev(a), dev(b)), (Some(x), Some(y)) if x == y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        exe: Option<&'static str>,
        writable: bool,
        cache: Option<&'static str>,
        same_volume: bool,
    }

    impl Fake {
        fn new(exe: Option<&'static str>, writable: bool) -> Self {
            Fake { exe, writable, cache: Some(CACHE), same_volume: true }
        }
    }
    impl InstallProbe for Fake {
        fn current_exe(&self) -> Option<PathBuf> {
            self.exe.map(PathBuf::from)
        }
        fn is_dir_writable(&self, _: &Path) -> bool {
            self.writable
        }
        fn cache_dir(&self) -> Option<PathBuf> {
            self.cache.map(PathBuf::from)
        }
        fn same_volume(&self, _: &Path, _: &Path) -> bool {
            self.same_volume
        }
    }

    const CACHE: &str = "/Users/me/Library/Caches/JAIT";
    const EXE: &str = "/Applications/JAIT.app/Contents/MacOS/JAIT";

    #[test]
    fn dev_build_is_disabled() {
        let f = Fake::new(Some("/repo/target/debug/JAIT"), true);
        assert_eq!(detect(&f), InstallContext::Disabled);
        let f = Fake::new(None, true);
        assert_eq!(detect(&f), InstallContext::Disabled);
    }

    #[test]
    fn translocated_is_dmg_only() {
        let f = Fake::new(
            Some("/private/var/folders/x/AppTranslocation/ABC/d/JAIT.app/Contents/MacOS/m"),
            true,
        );
        assert_eq!(detect(&f), InstallContext::DmgOnly(DmgReason::Translocated));
    }

    #[test]
    fn unwritable_parent_is_dmg_only() {
        let f = Fake::new(Some(EXE), false);
        assert_eq!(detect(&f), InstallContext::DmgOnly(DmgReason::FolderNotWritable));
    }

    #[test]
    fn normal_install_stages_in_the_cache_folder() {
        let install = detect(&Fake::new(Some(EXE), true));
        assert_eq!(
            install,
            InstallContext::SelfInstall {
                bundle: PathBuf::from("/Applications/JAIT.app"),
                staging_dir: PathBuf::from("/Users/me/Library/Caches/JAIT/update"),
            }
        );
        assert_eq!(
            install.legacy_staging_dir(),
            Some(PathBuf::from("/Applications/.jait-update"))
        );
    }

    #[test]
    fn cache_on_another_volume_stages_beside_bundle() {
        let mut f = Fake::new(Some(EXE), true);
        f.same_volume = false;
        let install = detect(&f);
        assert_eq!(
            install,
            InstallContext::SelfInstall {
                bundle: PathBuf::from("/Applications/JAIT.app"),
                staging_dir: PathBuf::from("/Applications/.jait-update"),
            }
        );
        assert_eq!(install.legacy_staging_dir(), None);
    }

    #[test]
    fn no_cache_folder_stages_beside_bundle() {
        let mut f = Fake::new(Some(EXE), true);
        f.cache = None;
        assert_eq!(detect(&f).staging_dir(), Some(Path::new("/Applications/.jait-update")));
    }

    #[test]
    fn system_probe_same_volume_walks_up_to_an_existing_folder() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/install-probe");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(SystemProbe.same_volume(&dir.join("not/yet/made"), &dir));
    }

    #[test]
    fn system_probe_checks_writability_in_target_dir() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/install-probe");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(SystemProbe.is_dir_writable(&dir));
        assert!(!SystemProbe.is_dir_writable(&dir.join("missing")));
    }
}
