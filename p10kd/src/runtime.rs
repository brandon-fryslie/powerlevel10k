//! Where one user's daemon lives on disk: `<base>/<build id>/`, where `<base>` is the
//! directory the shell names (`$TMPDIR/p10kd-$EUID`) and the build id is this binary's
//! ([`BUILD_ID`]). A rebuilt p10kd therefore gets its own daemon instead of talking to
//! one that speaks an older protocol.
//!
//! [LAW:one-source-of-truth] every path the shell, the attach helper and the daemon
//! agree on is derived here from the one directory.
//!
//! [LAW:parse-dont-validate] a [`RuntimeDir`] exists only for a directory this process
//! made safe: both levels are real directories (not symlinks) owned by the effective
//! user with no group or world access. The shell evaluates what arrives through this
//! directory, so a directory someone else planted is refused, loudly.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const BUILD_ID: &str = env!("P10KD_BUILD_ID");

#[derive(Debug, Clone)]
pub struct RuntimeDir {
    base: PathBuf,
    dir: PathBuf,
}

impl RuntimeDir {
    pub fn establish(base: &Path) -> io::Result<RuntimeDir> {
        secure_dir(base)?;
        let dir = base.join(BUILD_ID);
        secure_dir(&dir)?;
        Ok(RuntimeDir { base: base.to_path_buf(), dir })
    }

    /// The directory the shell named, which `serve` is given to re-establish.
    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Unix socket the attach helper talks to. Bound and connected relative to the
    /// directory ([`RuntimeDir::enter`]) because `sockaddr_un` caps the path at about a
    /// hundred bytes and `$TMPDIR` alone can be longer than that.
    pub const SOCKET_NAME: &'static str = "sock";

    /// Makes the directory the process's cwd so [`Self::SOCKET_NAME`] resolves inside it.
    pub fn enter(&self) -> io::Result<()> {
        std::env::set_current_dir(&self.dir)
    }

    /// Held for the daemon's whole life; whoever holds it is the daemon.
    pub fn run_lock(&self) -> PathBuf {
        self.dir.join("run.lock")
    }

    /// Held while a daemon starts or stops, so a starter never observes a half-built or
    /// half-torn-down daemon.
    pub fn start_lock(&self) -> PathBuf {
        self.dir.join("start.lock")
    }

    pub fn log(&self) -> PathBuf {
        self.dir.join("log")
    }

    /// Shell → daemon FIFO for the shell with this pid.
    pub fn req_fifo(&self, pid: u32) -> PathBuf {
        self.dir.join(format!("{pid}.req"))
    }

    /// Daemon → shell FIFO for the shell with this pid.
    pub fn resp_fifo(&self, pid: u32) -> PathBuf {
        self.dir.join(format!("{pid}.resp"))
    }
}

/// Creates `path` if absent and proves it is ours: a directory, not a symlink, owned by
/// the effective user, mode 0700 (set here when we own it with a looser mode).
fn secure_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(path)?;
    let refuse = |what: &str| {
        io::Error::other(format!("{} {what}; remove it or point TMPDIR elsewhere", path.display()))
    };
    if !meta.file_type().is_dir() {
        return Err(refuse("is not a directory"));
    }
    // SAFETY: geteuid cannot fail.
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(refuse(&format!("is owned by uid {}, not {euid}", meta.uid())));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("p10kd-rt-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn establishes_base_and_build_subdir_as_private_directories() {
        let base = scratch("ok");
        let rt = RuntimeDir::establish(&base).unwrap();
        assert_eq!(rt.path(), base.join(BUILD_ID));
        assert_eq!(rt.base(), base);
        for p in [&base, rt.path()] {
            assert_eq!(fs::metadata(p).unwrap().mode() & 0o777, 0o700, "{}", p.display());
        }
        // A loose mode on our own directory is tightened.
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        RuntimeDir::establish(&base).unwrap();
        assert_eq!(fs::metadata(&base).unwrap().mode() & 0o777, 0o700);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn refuses_a_symlink_and_a_file_in_place_of_the_directory() {
        let base = scratch("link");
        let target = scratch("target");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &base).unwrap();
        let err = RuntimeDir::establish(&base).unwrap_err();
        assert!(err.to_string().contains("is not a directory"), "{err}");
        fs::remove_file(&base).unwrap();
        fs::remove_dir_all(&target).unwrap();

        let file = scratch("file");
        fs::write(&file, "").unwrap();
        let err = RuntimeDir::establish(&file).unwrap_err();
        assert!(err.to_string().contains("is not a directory"), "{err}");
        fs::remove_file(&file).unwrap();
    }
}
