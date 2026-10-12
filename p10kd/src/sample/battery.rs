//! Facts: `[percent, status, remain]` for the internal battery, or `["", "", ""]` when
//! there is none.
//!
//! - `status` is one of `charging`, `discharging`, `full`: the OS vocabulary normalized
//!   at this boundary so the shell has one rule for turning it into a segment state.
//! - `remain` is `H:MM`, `...` while the OS is still estimating, or empty.
//!
//! Only macOS asks the daemon: `pmset` was the fork this replaces. On Linux the shell
//! reads `/sys/class/power_supply` itself, so there is nothing to serve.

use crate::protocol::Facts;

use super::SampleResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub status: Status,
    pub remain: Remain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Charging,
    Discharging,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remain {
    Unknown,
    Estimating,
    Minutes(u32),
}

pub fn sample() -> SampleResult {
    Ok(match platform::internal_battery()? {
        Some(b) => b.facts(),
        None => Facts::new(["", "", ""]),
    })
}

impl Battery {
    fn facts(&self) -> Facts {
        let status = match self.status {
            Status::Charging => "charging",
            Status::Discharging => "discharging",
            Status::Full => "full",
        };
        let remain = match self.remain {
            Remain::Unknown => String::new(),
            Remain::Estimating => "...".to_owned(),
            Remain::Minutes(m) => format!("{}:{:02}", m / 60, m % 60),
        };
        Facts::new([self.percent.to_string(), status.to_owned(), remain])
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    use super::super::SampleError;
    use super::{Battery, Remain, Status};

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> core_foundation::base::CFTypeRef;
        fn IOPSCopyPowerSourcesList(blob: core_foundation::base::CFTypeRef) -> core_foundation::array::CFArrayRef;
        fn IOPSGetPowerSourceDescription(
            blob: core_foundation::base::CFTypeRef,
            ps: core_foundation::base::CFTypeRef,
        ) -> core_foundation::dictionary::CFDictionaryRef;
    }

    /// The first power source whose Type is InternalBattery, read through IOKit's
    /// power-source API: the same data `pmset -g batt` prints.
    pub fn internal_battery() -> Result<Option<Battery>, SampleError> {
        // SAFETY: plain IOKit calls; every returned ref is checked for null before use and
        // the Copy* results are released by the wrappers (create rule).
        unsafe {
            let blob = IOPSCopyPowerSourcesInfo();
            if blob.is_null() {
                return Err(SampleError("IOPSCopyPowerSourcesInfo returned null".into()));
            }
            let blob: CFType = CFType::wrap_under_create_rule(blob);
            let list = IOPSCopyPowerSourcesList(blob.as_CFTypeRef());
            if list.is_null() {
                return Err(SampleError("IOPSCopyPowerSourcesList returned null".into()));
            }
            let list: CFArray<CFType> = CFArray::wrap_under_create_rule(list);
            for ps in list.iter() {
                let desc = IOPSGetPowerSourceDescription(blob.as_CFTypeRef(), ps.as_CFTypeRef());
                if desc.is_null() {
                    continue;
                }
                let desc: CFDictionary<CFString, CFType> = CFDictionary::wrap_under_get_rule(desc);
                if string(&desc, "Type").as_deref() == Some("InternalBattery") {
                    return Ok(Some(from_description(&desc)?));
                }
            }
            Ok(None)
        }
    }

    fn from_description(desc: &CFDictionary<CFString, CFType>) -> Result<Battery, SampleError> {
        let current = number(desc, "Current Capacity").ok_or_else(|| SampleError("battery lacks Current Capacity".into()))?;
        let max = number(desc, "Max Capacity").ok_or_else(|| SampleError("battery lacks Max Capacity".into()))?;
        let percent = if max > 0 { (current * 100 / max).clamp(0, 100) as u8 } else { 0 };
        let charged = boolean(desc, "Is Charged").unwrap_or(false);
        let charging = boolean(desc, "Is Charging").unwrap_or(false);
        let on_battery = string(desc, "Power Source State").as_deref() == Some("Battery Power");
        let status = if on_battery {
            Status::Discharging
        } else if charged || (!charging && percent == 100) {
            Status::Full
        } else {
            Status::Charging
        };
        let estimate = match status {
            Status::Discharging => number(desc, "Time to Empty"),
            Status::Charging => number(desc, "Time to Full Charge"),
            Status::Full => None,
        };
        let remain = match estimate {
            None | Some(0) => Remain::Unknown,
            Some(m) if m < 0 => Remain::Estimating,
            Some(m) => Remain::Minutes(m as u32),
        };
        Ok(Battery { percent, status, remain })
    }

    fn string(desc: &CFDictionary<CFString, CFType>, key: &str) -> Option<String> {
        desc.find(CFString::new(key)).and_then(|v| v.downcast::<CFString>()).map(|s| s.to_string())
    }

    fn number(desc: &CFDictionary<CFString, CFType>, key: &str) -> Option<i64> {
        desc.find(CFString::new(key)).and_then(|v| v.downcast::<CFNumber>()).and_then(|n| n.to_i64())
    }

    fn boolean(desc: &CFDictionary<CFString, CFType>, key: &str) -> Option<bool> {
        desc.find(CFString::new(key)).and_then(|v| v.downcast::<CFBoolean>()).map(bool::from)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::super::SampleError;
    use super::Battery;

    pub fn internal_battery() -> Result<Option<Battery>, SampleError> {
        Err(SampleError("battery is read from /sys by the shell on linux; the daemon does not serve it".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facts_render_status_and_remaining_time() {
        let b = Battery { percent: 87, status: Status::Discharging, remain: Remain::Minutes(125) };
        assert_eq!(b.facts(), Facts::new(["87", "discharging", "2:05"]));
        let b = Battery { percent: 100, status: Status::Full, remain: Remain::Unknown };
        assert_eq!(b.facts(), Facts::new(["100", "full", ""]));
        let b = Battery { percent: 5, status: Status::Charging, remain: Remain::Estimating };
        assert_eq!(b.facts(), Facts::new(["5", "charging", "..."]));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_sample_is_three_consistent_facts() {
        let facts = sample().unwrap();
        assert_eq!(facts.0.len(), 3);
        if !facts.0[0].is_empty() {
            let pct: u8 = facts.0[0].parse().unwrap();
            assert!(pct <= 100);
            assert!(["charging", "discharging", "full"].contains(&facts.0[1].as_str()));
        } else {
            assert_eq!(facts, Facts::new(["", "", ""]));
        }
    }
}
