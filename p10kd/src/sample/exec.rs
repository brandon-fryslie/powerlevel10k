//! Facts that are the first line of a program's output. These forks are the fact itself:
//! `rustc --version` is how rustup resolves the toolchain for a directory, and the gcloud
//! project name lives behind an API call only `gcloud` can make.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::protocol::Facts;

use super::{SampleError, SampleResult};

/// Facts: `[version]` — `1.97.1` from `rustc 1.97.1 (…)`, or empty when rustc fails.
pub fn rust_version(rustc: &Path, cwd: &Path) -> SampleResult {
    let version = first_line(Command::new(rustc).arg("--version").current_dir(cwd))?
        .map(|line| line.strip_prefix("rustc ").unwrap_or(&line).split(' ').next().unwrap_or("").to_owned())
        .unwrap_or_default();
    Ok(Facts::new([version]))
}

/// Facts: `[name]` — the project's display name, or empty when gcloud fails.
pub fn gcloud_project_name(gcloud: &Path, configuration: &str, account: &str, project_id: &str) -> SampleResult {
    let name = first_line(
        Command::new(gcloud)
            .args(["projects", "describe", project_id])
            .arg(format!("--configuration={configuration}"))
            .arg(format!("--account={account}"))
            .arg("--format=value(name)"),
    )?
    .unwrap_or_default();
    Ok(Facts::new([name]))
}

/// Runs the command; `Ok(None)` when it exits non-zero or cannot be spawned because the
/// program is missing or the directory is gone, which are states the segment renders as
/// "no value". Any other spawn failure is an error.
pub fn first_line(cmd: &mut Command) -> Result<Option<String>, SampleError> {
    let out = match cmd.stdin(Stdio::null()).stderr(Stdio::null()).output() {
        Ok(out) => out,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied) => {
            return Ok(None)
        }
        Err(e) => return Err(SampleError(format!("spawn {:?}: {e}", cmd.get_program()))),
    };
    if !out.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(Some(text.lines().next().unwrap_or("").to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_version_is_a_dotted_number_when_rustc_exists() {
        let facts = rust_version(Path::new("rustc"), Path::new("/")).unwrap();
        let v = &facts.0[0];
        assert!(v.split('.').count() == 3 && v.split('.').all(|p| p.parse::<u32>().is_ok()), "{v}");
    }

    #[test]
    fn missing_program_and_missing_cwd_are_empty_not_errors() {
        assert_eq!(rust_version(Path::new("/no/such/rustc"), Path::new("/")).unwrap(), Facts::new([""]));
        assert_eq!(rust_version(Path::new("rustc"), Path::new("/no/such/dir")).unwrap(), Facts::new([""]));
        assert_eq!(gcloud_project_name(Path::new("/no/such/gcloud"), "c", "a", "p").unwrap(), Facts::new([""]));
    }

    #[test]
    fn nonzero_exit_is_empty() {
        assert_eq!(first_line(&mut Command::new("false")).unwrap(), None);
        assert_eq!(first_line(Command::new("echo").args(["a", "b"])).unwrap(), Some("a b".into()));
    }
}
