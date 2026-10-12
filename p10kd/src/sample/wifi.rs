//! Facts: `[on, ssid, rssi, noise, tx_rate, auth]`.
//!
//! `on` is `1` while associated with a network and empty otherwise; when it is empty every
//! other fact is empty too. `rssi` and `noise` are dBm integers, `tx_rate` is Mbit/s,
//! `auth` names the security type the way the old `airport -I` did (`wpa2-psk`, …).
//!
//! On macOS 14.4+ the system withholds the SSID from processes without Location
//! permission; the daemon reports what it is given and `ssid` is then empty.

use crate::protocol::Facts;

use super::SampleResult;

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub ssid: String,
    pub rssi: i64,
    pub noise: i64,
    pub tx_rate: f64,
    pub auth: String,
}

pub fn sample() -> SampleResult {
    Ok(match platform::link()? {
        Some(link) => link.facts(),
        None => Facts::new(["", "", "", "", "", ""]),
    })
}

impl Link {
    fn facts(&self) -> Facts {
        Facts::new([
            "1".to_owned(),
            self.ssid.clone(),
            self.rssi.to_string(),
            self.noise.to_string(),
            trim_rate(self.tx_rate),
            self.auth.clone(),
        ])
    }
}

/// `866` for 866.0, `1.5` for 1.5: the shell's `${${rate%%0#}%.}` trimming.
fn trim_rate(rate: f64) -> String {
    if rate.fract() == 0.0 {
        format!("{rate:.0}")
    } else {
        format!("{rate}")
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_core_wlan::{CWSecurity, CWWiFiClient};

    use super::super::SampleError;
    use super::Link;

    pub fn link() -> Result<Option<Link>, SampleError> {
        // SAFETY: CoreWLAN's shared client and its properties are plain getters with no
        // preconditions; the Retained wrappers manage the object lifetimes.
        unsafe {
            let client = CWWiFiClient::sharedWiFiClient();
            let Some(iface) = client.interface() else {
                return Ok(None);
            };
            if !iface.powerOn() {
                return Ok(None);
            }
            let ssid = iface.ssid().map(|s| s.to_string());
            let rssi = iface.rssiValue() as i64;
            let associated = ssid.is_some() || rssi != 0;
            if !associated {
                return Ok(None);
            }
            Ok(Some(Link {
                ssid: ssid.unwrap_or_default(),
                rssi,
                noise: iface.noiseMeasurement() as i64,
                tx_rate: iface.transmitRate(),
                auth: auth_name(iface.security()).to_owned(),
            }))
        }
    }

    /// `airport -I` spellings for CoreWLAN's security types.
    fn auth_name(security: CWSecurity) -> &'static str {
        match security {
            CWSecurity::None => "none",
            CWSecurity::WEP => "wep",
            CWSecurity::WPAPersonal => "wpa-psk",
            CWSecurity::WPAPersonalMixed => "wpa/wpa2-psk",
            CWSecurity::WPA2Personal | CWSecurity::Personal => "wpa2-psk",
            CWSecurity::DynamicWEP => "dynamic-wep",
            CWSecurity::WPAEnterprise => "wpa-eap",
            CWSecurity::WPAEnterpriseMixed => "wpa/wpa2-eap",
            CWSecurity::WPA2Enterprise | CWSecurity::Enterprise => "wpa2-eap",
            CWSecurity::WPA3Personal => "wpa3-sae",
            CWSecurity::WPA3Enterprise => "wpa3-eap",
            CWSecurity::WPA3Transition => "wpa2/wpa3-psk",
            _ => "",
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::process::Command;

    use super::super::SampleError;
    use super::Link;

    /// `/proc/net/wireless` for the interface, signal and noise; `iw dev <if> link` for
    /// the SSID and transmit rate, exactly the two sources the shell used to parse.
    pub fn link() -> Result<Option<Link>, SampleError> {
        let wireless = match fs::read_to_string("/proc/net/wireless") {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let rows: Vec<&str> = wireless.lines().filter(|l| !l.contains('|')).collect();
        let [row] = rows[..] else {
            return Ok(None);
        };
        let parts: Vec<&str> = row.split_whitespace().collect();
        let (Some(iface), Some(status), Some(level), Some(noise)) =
            (parts.first(), parts.get(1), parts.get(3), parts.get(4))
        else {
            return Ok(None);
        };
        let iface = iface.trim_end_matches(':');
        let dbm = |s: &str| s.trim_end_matches('.').parse::<i64>().ok();
        let (Some(rssi), Some(noise)) = (dbm(level), dbm(noise)) else {
            return Ok(None);
        };
        if !status.chars().all(|c| c == '0') {
            return Ok(None);
        }
        let out = Command::new("iw").args(["dev", iface, "link"]).stdin(std::process::Stdio::null()).output()?;
        if !out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut ssid = None;
        let mut tx_rate = None;
        for line in text.lines().map(str::trim_start) {
            if let Some(s) = line.strip_prefix("SSID:") {
                ssid = Some(s.trim().to_owned());
            } else if let Some(r) = line.strip_prefix("tx bitrate:") {
                tx_rate = r.trim().split(' ').next().and_then(|v| v.parse::<f64>().ok());
            }
        }
        Ok(match (ssid, tx_rate) {
            (Some(ssid), Some(tx_rate)) => Some(Link { ssid, rssi, noise, tx_rate, auth: String::new() }),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_are_on_plus_link_fields() {
        let l = Link { ssid: "Home".into(), rssi: -52, noise: -90, tx_rate: 866.0, auth: "wpa2-psk".into() };
        assert_eq!(l.facts(), Facts::new(["1", "Home", "-52", "-90", "866", "wpa2-psk"]));
        assert_eq!(trim_rate(1.5), "1.5");
    }

    #[test]
    fn sample_is_six_facts_and_off_means_all_empty() {
        let facts = sample().unwrap();
        assert_eq!(facts.0.len(), 6);
        if facts.0[0].is_empty() {
            assert!(facts.0.iter().all(String::is_empty));
        } else {
            assert_eq!(facts.0[0], "1");
            facts.0[2].parse::<i64>().unwrap();
            facts.0[3].parse::<i64>().unwrap();
        }
    }
}
