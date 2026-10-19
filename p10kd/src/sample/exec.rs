//! Facts that are the first line of a program's output. These forks are the fact itself:
//! `rustc --version` is how rustup resolves the toolchain for a directory, and the gcloud
//! project name lives behind an API call only `gcloud` can make.
//!
//! Every program runs in the environment the asking shell sent ([`Env`]) and nothing
//! else: the daemon's own environment belongs to whichever shell started it, and a
//! toolchain override or proxy setting is a fact about the shell that is asking.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::protocol::{Env, Facts};

use super::{SampleError, SampleResult};

/// A command that runs with exactly `env`. `PATH` from `env` is what resolves a bare
/// program name.
pub fn command(program: impl AsRef<OsStr>, env: &Env) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear().envs(env.iter());
    cmd
}

/// Facts: `[version]` — `1.97.1` from `rustc 1.97.1 (…)`, or empty when rustc fails.
pub fn rust_version(env: &Env, rustc: &Path, cwd: &Path) -> SampleResult {
    let version = first_line(command(rustc, env).arg("--version").current_dir(cwd))?
        .map(|line| line.strip_prefix("rustc ").unwrap_or(&line).split(' ').next().unwrap_or("").to_owned())
        .unwrap_or_default();
    Ok(Facts::new([version]))
}

/// Facts: `[name]` — the project's display name, or empty when gcloud fails.
pub fn gcloud_project_name(
    env: &Env,
    gcloud: &Path,
    configuration: &OsStr,
    account: &OsStr,
    project_id: &OsStr,
) -> SampleResult {
    let flag = |name: &str, value: &OsStr| {
        let mut flag = std::ffi::OsString::from(name);
        flag.push(value);
        flag
    };
    let name = first_line(
        command(gcloud, env)
            .args(["projects", "describe"])
            .arg(project_id)
            .arg(flag("--configuration=", configuration))
            .arg(flag("--account=", account))
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

    fn env(entries: &[(&str, &str)]) -> Env {
        Env(entries.iter().map(|(k, v)| (k.into(), v.into())).collect())
    }

    fn host_env() -> Env {
        let keep = ["PATH", "HOME", "RUSTUP_HOME", "CARGO_HOME"];
        Env(std::env::vars_os().filter(|(k, _)| keep.iter().any(|w| k == w)).collect())
    }

    #[test]
    fn rust_version_is_a_dotted_number_when_rustc_exists() {
        let facts = rust_version(&host_env(), Path::new("rustc"), Path::new("/")).unwrap();
        let v = &facts.0[0];
        assert!(v.split('.').count() == 3 && v.split('.').all(|p| p.parse::<u32>().is_ok()), "{v}");
    }

    #[test]
    fn missing_program_and_missing_cwd_are_empty_not_errors() {
        let env = host_env();
        assert_eq!(rust_version(&env, Path::new("/no/such/rustc"), Path::new("/")).unwrap(), Facts::new([""]));
        assert_eq!(rust_version(&env, Path::new("rustc"), Path::new("/no/such/dir")).unwrap(), Facts::new([""]));
        let (c, a, p) = (OsStr::new("c"), OsStr::new("a"), OsStr::new("p"));
        assert_eq!(gcloud_project_name(&env, Path::new("/no/such/gcloud"), c, a, p).unwrap(), Facts::new([""]));
    }

    #[test]
    fn nonzero_exit_is_empty() {
        let env = host_env();
        assert_eq!(first_line(&mut command("false", &env)).unwrap(), None);
        assert_eq!(first_line(command("echo", &env).args(["a", "b"])).unwrap(), Some("a b".into()));
    }

    #[test]
    fn program_sees_only_the_sent_environment_and_is_found_through_its_path() {
        let dir = std::env::temp_dir().join(format!("p10kd-exec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("probe");
        std::fs::write(&script, "#!/bin/sh\necho \"$MARK/${HOME:-unset}\"\n").unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let sent = env(&[("PATH", dir.to_str().unwrap()), ("MARK", "m1")]);
        assert_eq!(first_line(&mut command("probe", &sent)).unwrap(), Some("m1/unset".into()));
        assert_eq!(first_line(&mut command("probe", &env(&[("MARK", "m2")]))).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
