//! Facts: `[ip]` — the machine's public address as seen from outside, or empty when no
//! method produced one. Methods are the tools the shell was configured with
//! (`POWERLEVEL9K_PUBLIC_IP_METHODS`), tried in order and then once more over; a tool
//! that is not installed is skipped.

use std::process::Command;

use crate::protocol::{Facts, PublicIpMethod};

use super::exec::first_line;
use super::SampleResult;

pub fn sample(host: &str, methods: &[PublicIpMethod]) -> SampleResult {
    for method in methods.iter().chain(methods) {
        if let Some(ip) = fetch(host, *method)?.filter(|ip| is_ip_shaped(ip)) {
            return Ok(Facts::new([ip]));
        }
    }
    Ok(Facts::new([""]))
}

fn fetch(host: &str, method: PublicIpMethod) -> Result<Option<String>, super::SampleError> {
    Ok(match method {
        PublicIpMethod::Dig => {
            let v4 = dig("-4", "A")?.filter(|s| !s.starts_with(';'));
            match v4 {
                Some(ip) if !ip.is_empty() => Some(ip),
                _ => dig("-6", "AAAA")?.filter(|s| !s.starts_with(';')),
            }
        }
        PublicIpMethod::Curl => first_line(Command::new("curl").args(["--max-time", "5", "-w", "\n", host]))?,
        PublicIpMethod::Wget => first_line(Command::new("wget").args(["-T", "5", "-qO-", host]))?,
    })
}

fn dig(family: &str, rrtype: &str) -> Result<Option<String>, super::SampleError> {
    first_line(Command::new("dig").args([
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
        assert_eq!(sample("https://example.invalid/", &[]).unwrap(), Facts::new([""]));
    }
}
