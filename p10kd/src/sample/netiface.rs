//! Facts: `[name, ipv4, rx_bytes, tx_bytes]` repeated once per interface that is UP and
//! has an IPv4 address: the first IPv4 address of each, in the kernel's interface order,
//! as `ifconfig`/`ip -4 a` listed them. Byte counters are empty when the kernel does not
//! report them for that interface. Which interfaces matter is the shell's decision: it
//! matches these names against the user's `*_INTERFACE` patterns.

use std::ffi::CStr;
use std::io;
use std::net::Ipv4Addr;

use crate::protocol::Facts;

use super::{SampleError, SampleResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub ip: Ipv4Addr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub fn sample() -> SampleResult {
    let counters = platform::counters()?;
    let mut facts = Vec::new();
    for iface in up_ipv4_interfaces()? {
        let c = counters.get(&iface.name).copied();
        facts.push(iface.name);
        facts.push(iface.ip.to_string());
        facts.push(c.map(|c| c.rx_bytes.to_string()).unwrap_or_default());
        facts.push(c.map(|c| c.tx_bytes.to_string()).unwrap_or_default());
    }
    Ok(Facts(facts))
}

/// One entry per UP interface with an IPv4 address, carrying its first such address.
fn up_ipv4_interfaces() -> Result<Vec<Interface>, SampleError> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list`, which we free below with freeifaddrs.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Err(SampleError(format!("getifaddrs: {}", io::Error::last_os_error())));
    }
    let mut seen = Vec::new();
    let mut cur = list;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getifaddrs returned, valid until freeifaddrs.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || (ifa.ifa_flags & libc::IFF_UP as u32) == 0 {
            continue;
        }
        // SAFETY: ifa_addr is non-null; we only read the family before casting by family.
        if unsafe { (*ifa.ifa_addr).sa_family } as i32 != libc::AF_INET {
            continue;
        }
        // SAFETY: family is AF_INET so ifa_addr points at a sockaddr_in.
        let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        // SAFETY: ifa_name is a NUL-terminated string owned by the list.
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned();
        if seen.iter().any(|i: &Interface| i.name == name) {
            continue;
        }
        seen.push(Interface { name, ip });
    }
    // SAFETY: `list` came from getifaddrs and is not used after this.
    unsafe { libc::freeifaddrs(list) };
    Ok(seen)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::collections::BTreeMap;
    use std::ffi::CStr;
    use std::io;
    use std::mem::size_of;

    use super::super::SampleError;
    use super::Counters;

    /// 64-bit per-interface byte counters from the routing socket's interface list,
    /// the same source `netstat -ib` reads.
    pub fn counters() -> Result<BTreeMap<String, Counters>, SampleError> {
        let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
        let mut len = 0usize;
        // SAFETY: a null oldp with a len pointer asks sysctl for the needed buffer size.
        if unsafe { libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) } != 0 {
            return Err(SampleError(format!("sysctl NET_RT_IFLIST2 size: {}", io::Error::last_os_error())));
        }
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` has `len` bytes, which is what sysctl just asked for.
        if unsafe { libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } != 0 {
            return Err(SampleError(format!("sysctl NET_RT_IFLIST2: {}", io::Error::last_os_error())));
        }
        buf.truncate(len);
        let mut out = BTreeMap::new();
        let mut off = 0usize;
        while off + size_of::<libc::rt_msghdr>() <= buf.len() {
            // SAFETY: every record starts with rt_msghdr; read_unaligned tolerates packing.
            let hdr: libc::rt_msghdr = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
            let msglen = hdr.rtm_msglen as usize;
            if msglen == 0 || off + msglen > buf.len() {
                break;
            }
            if i32::from(hdr.rtm_type) == libc::RTM_IFINFO2 && msglen >= size_of::<libc::if_msghdr2>() {
                // SAFETY: the record is RTM_IFINFO2 and long enough to hold an if_msghdr2.
                let ifm: libc::if_msghdr2 = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
                let mut name = [0 as libc::c_char; libc::IF_NAMESIZE];
                // SAFETY: `name` is IF_NAMESIZE bytes, the length if_indextoname requires.
                if !unsafe { libc::if_indextoname(u32::from(ifm.ifm_index), name.as_mut_ptr()) }.is_null() {
                    // SAFETY: if_indextoname NUL-terminated `name`.
                    let name = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
                    let data = ifm.ifm_data;
                    out.insert(name, Counters { rx_bytes: data.ifi_ibytes, tx_bytes: data.ifi_obytes });
                }
            }
            off += msglen;
        }
        Ok(out)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::BTreeMap;
    use std::fs;

    use super::super::SampleError;
    use super::Counters;

    /// `/sys/class/net/<if>/statistics/{rx,tx}_bytes`, the 64-bit counters.
    pub fn counters() -> Result<BTreeMap<String, Counters>, SampleError> {
        let mut out = BTreeMap::new();
        for entry in fs::read_dir("/sys/class/net")? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let read = |f: &str| -> Option<u64> {
                fs::read_to_string(entry.path().join("statistics").join(f)).ok()?.trim().parse().ok()
            };
            if let (Some(rx_bytes), Some(tx_bytes)) = (read("rx_bytes"), read("tx_bytes")) {
                out.insert(name, Counters { rx_bytes, tx_bytes });
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_listed_once_with_its_address() {
        let ifaces = up_ipv4_interfaces().unwrap();
        let lo: Vec<_> = ifaces.iter().filter(|i| i.ip.is_loopback()).collect();
        assert_eq!(lo.len(), 1, "{ifaces:?}");
    }

    #[test]
    fn facts_come_in_quads_with_numeric_counters() {
        let facts = sample().unwrap();
        assert_eq!(facts.0.len() % 4, 0);
        for quad in facts.0.chunks(4) {
            assert!(!quad[0].is_empty());
            quad[1].parse::<Ipv4Addr>().unwrap();
            for c in &quad[2..] {
                assert!(c.is_empty() || c.parse::<u64>().is_ok(), "{c}");
            }
        }
        let counters = platform::counters().unwrap();
        assert!(!counters.is_empty());
    }
}
