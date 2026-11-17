//! Where the app runs from, and whether it can replace itself there.
//! The decision is pure; only `InstallProbe` touches the system.

use std::path::{Path, PathBuf};

/// Name of the hidden staging folder created beside the bundle.
pub const STAGING_DIR_NAME: &str = ".jait-update";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallContext {
    /// No enclosing `.app` (e.g. `cargo run`): the updater does nothing.
    Disabled,
    /// Can't self-install; offer the DMG instead.
    DmgOnly(DmgReason),
    /// Self-install possible; staging lives beside the bundle (same volume).
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
    let staging_dir = parent.join(STAGING_DIR_NAME);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        exe: Option<&'static str>,
        writable: bool,
    }
    impl InstallProbe for Fake {
        fn current_exe(&self) -> Option<PathBuf> {
            self.exe.map(PathBuf::from)
        }
        fn is_dir_writable(&self, _: &Path) -> bool {
            self.writable
        }
    }

    const EXE: &str = "/Applications/JAIT.app/Contents/MacOS/JAIT";

    #[test]
    fn dev_build_is_disabled() {
        let f = Fake { exe: Some("/repo/target/debug/JAIT"), writable: true };
        assert_eq!(detect(&f), InstallContext::Disabled);
        let f = Fake { exe: None, writable: true };
        assert_eq!(detect(&f), InstallContext::Disabled);
    }

    #[test]
    fn translocated_is_dmg_only() {
        let f = Fake {
            exe: Some("/private/var/folders/x/AppTranslocation/ABC/d/JAIT.app/Contents/MacOS/m"),
            writable: true,
        };
        assert_eq!(detect(&f), InstallContext::DmgOnly(DmgReason::Translocated));
    }

    #[test]
    fn unwritable_parent_is_dmg_only() {
        let f = Fake { exe: Some(EXE), writable: false };
        assert_eq!(detect(&f), InstallContext::DmgOnly(DmgReason::FolderNotWritable));
    }

    #[test]
    fn normal_install_stages_beside_bundle() {
        let f = Fake { exe: Some(EXE), writable: true };
        assert_eq!(
            detect(&f),
            InstallContext::SelfInstall {
                bundle: PathBuf::from("/Applications/JAIT.app"),
                staging_dir: PathBuf::from("/Applications/.jait-update"),
            }
        );
    }

    #[test]
    fn system_probe_checks_writability_in_target_dir() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp/install-probe");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(SystemProbe.is_dir_writable(&dir));
        assert!(!SystemProbe.is_dir_writable(&dir.join("missing")));
    }
}
