//! `flock(2)` as a value: the lock is held exactly as long as the [`FileLock`] lives, and
//! it survives `fork` because it belongs to the open file description, which is what lets
//! the daemon's parent take the lock and the detached child keep it.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub struct FileLock {
    _file: File,
}

impl FileLock {
    /// Blocks until the lock is held.
    pub fn acquire(path: &Path) -> io::Result<FileLock> {
        let file = open(path)?;
        flock(&file, libc::LOCK_EX)?;
        Ok(FileLock { _file: file })
    }

    /// `Ok(None)` when someone else holds it.
    pub fn try_acquire(path: &Path) -> io::Result<Option<FileLock>> {
        let file = open(path)?;
        match flock(&file, libc::LOCK_EX | libc::LOCK_NB) {
            Ok(()) => Ok(Some(FileLock { _file: file })),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

fn flock(file: &File, op: libc::c_int) -> io::Result<()> {
    // SAFETY: `file` is an open descriptor for the lifetime of this call.
    if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_is_refused_until_the_first_drops() {
        let dir = std::env::temp_dir().join(format!("p10kd-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("l");
        let first = FileLock::acquire(&path).unwrap();
        assert!(FileLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        // A process another test spawns while `first` is held shares its file description
        // until that child execs (about a millisecond), so the release is observed with a
        // bounded wait rather than instantly.
        let t0 = std::time::Instant::now();
        let mut second = FileLock::try_acquire(&path).unwrap();
        while second.is_none() && t0.elapsed() < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(1));
            second = FileLock::try_acquire(&path).unwrap();
        }
        assert!(second.is_some(), "lock still held {:?} after release", t0.elapsed());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

