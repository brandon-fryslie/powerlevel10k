//! Facts: `[pct]` — percentage of the filesystem holding `path` that is in use, the
//! way `df -P` reports it (used / (used + available), rounded up). `pct` is empty when
//! the path no longer exists: the shell's cwd can be deleted under it, and that is a
//! legitimate state, not a sampler failure.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::protocol::Facts;

use super::SampleResult;

pub fn sample(path: &Path) -> SampleResult {
    let pct = match statvfs(path) {
        Ok(st) => {
            let used = st.f_blocks.saturating_sub(st.f_bfree) as u128;
            let avail = st.f_bavail as u128;
            percent_used(used, avail).to_string()
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(super::SampleError(format!("statvfs {}: {e}", path.display()))),
    };
    Ok(Facts::new([pct]))
}

fn statvfs(path: &Path) -> io::Result<libc::statvfs> {
    let c = CString::new(path.as_os_str().as_bytes())?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a valid NUL-terminated path and `st` is writable storage of the right type.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs returned 0, so it initialized `st`.
    Ok(unsafe { st.assume_init() })
}

/// `df -P`'s capacity column: rounded up, so a filesystem with any usage never shows 0%.
fn percent_used(used: u128, avail: u128) -> u128 {
    let total = used + avail;
    if total == 0 {
        return 0;
    }
    (used * 100).div_ceil(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_rounds_up_like_df() {
        assert_eq!(percent_used(0, 0), 0);
        assert_eq!(percent_used(0, 100), 0);
        assert_eq!(percent_used(1, 999), 1);
        assert_eq!(percent_used(50, 50), 50);
        assert_eq!(percent_used(999, 1), 100);
        assert_eq!(percent_used(100, 0), 100);
    }

    #[test]
    fn root_is_a_number_and_a_missing_path_is_empty() {
        let facts = sample(Path::new("/")).unwrap();
        let pct: u8 = facts.0[0].parse().unwrap();
        assert!(pct <= 100);
        assert_eq!(sample(Path::new("/definitely/not/here")).unwrap(), Facts::new([""]));
    }
}
