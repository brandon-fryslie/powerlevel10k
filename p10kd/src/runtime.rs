//! Where one user's daemon lives on disk. [LAW:one-source-of-truth] every path the shell,
//! the attach helper and the daemon agree on is derived here from the one directory.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RuntimeDir {
    dir: PathBuf,
}

impl RuntimeDir {
    pub fn new(dir: PathBuf) -> Self {
        RuntimeDir { dir }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Unix socket the attach helper talks to. Bound and connected relative to the
    /// directory ([`RuntimeDir::enter`]) because `sockaddr_un` caps the path at about a
    /// hundred bytes and `$TMPDIR` alone can be longer than that.
    pub const SOCKET_NAME: &'static str = "sock";

    pub fn socket(&self) -> PathBuf {
        self.dir.join(Self::SOCKET_NAME)
    }

    /// Makes the directory the process's cwd so [`Self::SOCKET_NAME`] resolves inside it.
    pub fn enter(&self) -> std::io::Result<()> {
        std::env::set_current_dir(&self.dir)
    }

    /// Held for the daemon's whole life; whoever holds it is the daemon.
    pub fn run_lock(&self) -> PathBuf {
        self.dir.join("run.lock")
    }

    /// Held while the daemon is starting or stopping, so a starter never observes a
    /// half-built or half-torn-down daemon.
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
