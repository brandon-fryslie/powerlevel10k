//! Facts: `[ip]` — the machine's public address as seen from outside, or empty when no
//! method produced one. Methods are the tools the shell was configured with
//! (`POWERLEVEL9K_PUBLIC_IP_METHODS`), tried in order and then once more over; a tool
//! that is not installed is skipped.

use std::ffi::OsStr;

use crate::protocol::{Env, Facts, PublicIpMethod};

use super::exec::{command, first_line};
use super::SampleResult;

pub fn sample(env: &Env, host: &OsStr, methods: &[PublicIpMethod]) -> SampleResult {
    for method in methods.iter().chain(methods) {
        if let Some(ip) = fetch(env, host, *method)?.filter(|ip| is_ip_shaped(ip)) {
            return Ok(Facts::new([ip]));
        }
    }
    Ok(Facts::new([""]))
}

fn fetch(env: &Env, host: &OsStr, method: PublicIpMethod) -> Result<Option<String>, super::SampleError> {
    Ok(match method {
        PublicIpMethod::Dig => {
            let v4 = dig(env, "-4", "A")?.filter(|s| !s.starts_with(';'));
            match v4 {
                Some(ip) if !ip.is_empty() => Some(ip),
                _ => dig(env, "-6", "AAAA")?.filter(|s| !s.starts_with(';')),
            }
        }
        PublicIpMethod::Curl => first_line(command("curl", env).args(["--max-time", "5", "-w", "\n"]).arg(host))?,
        PublicIpMethod::Wget => first_line(command("wget", env).args(["-T", "5", "-qO-"]).arg(host))?,
    })
}

fn dig(env: &Env, family: &str, rrtype: &str) -> Result<Option<String>, super::SampleError> {
    first_line(command("dig", env).args([
        "+tries=1",
        "+short",
        family,
        rrtype,
        "myip.opendns.com",
        "@resolver1.opendns.com",
    ]))
}

/// The shell accepted `^[0-9a-f.:]+$`; so do we.
fn is_ip_shaped(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase() || c == '.' || c == ':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_shape() {
        assert!(is_ip_shaped("203.0.113.9"));
        assert!(is_ip_shaped("2001:db8::1"));
        assert!(!is_ip_shaped(""));
        assert!(!is_ip_shaped("<html>"));
        assert!(!is_ip_shaped("2001:DB8::1"));
    }

    #[test]
    fn no_methods_yields_empty() {
        assert_eq!(sample(&Env::default(), OsStr::new("https://example.invalid/"), &[]).unwrap(), Facts::new([""]));
    }
}
