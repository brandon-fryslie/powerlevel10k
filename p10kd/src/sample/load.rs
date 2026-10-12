//! Facts: `[one, five, fifteen]` — load averages, two decimals, as `sysctl vm.loadavg`
//! and `/proc/loadavg` print them.

use crate::protocol::Facts;

use super::{SampleError, SampleResult};

pub fn sample() -> SampleResult {
    let mut avg = [0f64; 3];
    // SAFETY: `avg` holds 3 doubles and we pass its length.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    if n != 3 {
        return Err(SampleError(format!("getloadavg returned {n}")));
    }
    Ok(Facts::new(avg.iter().map(|v| format!("{v:.2}"))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn three_nonnegative_decimals() {
        let facts = super::sample().unwrap();
        assert_eq!(facts.0.len(), 3);
        for f in &facts.0 {
            let v: f64 = f.parse().unwrap();
            assert!(v >= 0.0, "{f}");
            assert_eq!(f.split('.').nth(1).map(str::len), Some(2), "{f}");
        }
    }
}
