//! Facts: `[bytes]` — ram reports free bytes, swap reports used bytes. The shell renders
//! them with its own `_p9k_human_readable_bytes`, so its formatting stays the one source.

use crate::protocol::Facts;

use super::SampleResult;

pub fn ram() -> SampleResult {
    Ok(Facts::new([platform::ram_free_bytes()?.to_string()]))
}

pub fn swap() -> SampleResult {
    Ok(Facts::new([platform::swap_used_bytes()?.to_string()]))
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::CString;
    use std::io;
    use std::mem::{size_of, MaybeUninit};

    use super::super::SampleError;

    extern "C" {
        // libc deprecates its binding in favour of a crate we do not otherwise need.
        fn mach_host_self() -> libc::mach_port_t;
    }

    /// Free + inactive pages, as `vm_stat` reports them, times the page size.
    pub fn ram_free_bytes() -> Result<u64, SampleError> {
        let mut stats = MaybeUninit::<libc::vm_statistics64>::uninit();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `stats` is sized for HOST_VM_INFO64 and `count` says so.
        let rc = unsafe {
            libc::host_statistics64(
                mach_host_self(),
                libc::HOST_VM_INFO64,
                stats.as_mut_ptr() as libc::host_info64_t,
                &mut count,
            )
        };
        if rc != libc::KERN_SUCCESS {
            return Err(SampleError(format!("host_statistics64 failed: kern_return {rc}")));
        }
        // SAFETY: KERN_SUCCESS means the struct was filled.
        let stats = unsafe { stats.assume_init() };
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 {
            return Err(SampleError(format!("sysconf(_SC_PAGESIZE) returned {page}")));
        }
        let pages = u64::from(stats.free_count) + u64::from(stats.inactive_count);
        Ok(pages * page as u64)
    }

    pub fn swap_used_bytes() -> Result<u64, SampleError> {
        let name = CString::new("vm.swapusage").expect("static name");
        let mut usage = MaybeUninit::<libc::xsw_usage>::uninit();
        let mut len = size_of::<libc::xsw_usage>();
        // SAFETY: `usage` is `len` bytes of writable storage for exactly this sysctl's type.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                usage.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Err(SampleError(format!("sysctl vm.swapusage: {}", io::Error::last_os_error())));
        }
        // SAFETY: the sysctl succeeded and filled the struct.
        Ok(unsafe { usage.assume_init() }.xsu_used)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;

    use super::super::SampleError;

    /// `MemAvailable` when the kernel reports it, else `MemFree`.
    pub fn ram_free_bytes() -> Result<u64, SampleError> {
        let meminfo = fs::read_to_string("/proc/meminfo")?;
        let kib = field(&meminfo, "MemAvailable:")
            .or_else(|| field(&meminfo, "MemFree:"))
            .ok_or_else(|| SampleError("/proc/meminfo has neither MemAvailable nor MemFree".into()))?;
        Ok(kib * 1024)
    }

    pub fn swap_used_bytes() -> Result<u64, SampleError> {
        let meminfo = fs::read_to_string("/proc/meminfo")?;
        let total = field(&meminfo, "SwapTotal:")
            .ok_or_else(|| SampleError("/proc/meminfo lacks SwapTotal".into()))?;
        let free = field(&meminfo, "SwapFree:")
            .ok_or_else(|| SampleError("/proc/meminfo lacks SwapFree".into()))?;
        Ok(total.saturating_sub(free) * 1024)
    }

    fn field(meminfo: &str, key: &str) -> Option<u64> {
        meminfo
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn both_are_nonnegative_integers() {
        let ram: u64 = super::ram().unwrap().0[0].parse().unwrap();
        assert!(ram > 0, "a running machine has free memory");
        let _swap: u64 = super::swap().unwrap().0[0].parse().unwrap();
    }
}
