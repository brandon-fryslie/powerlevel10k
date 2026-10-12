//! The shell-side helper: `p10kd attach <dir> <pid>` returns 0 once `<dir>/<pid>.req`
//! and `<dir>/<pid>.resp` exist and can be opened without blocking. It starts the daemon
//! when none is listening, and tries once more if the daemon it reached was in the
//! middle of shutting down (its answer is then end-of-file rather than `ok`).

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

use crate::runtime::RuntimeDir;

pub fn attach(rt: &RuntimeDir, pid: u32) -> io::Result<()> {
    std::fs::create_dir_all(rt.path())?;
    rt.enter()?;
    for _ in 0..2 {
        match try_attach(pid)? {
            Outcome::Attached => return Ok(()),
            Outcome::NoDaemon => spawn_serve(rt)?,
        }
    }
    Err(io::Error::new(io::ErrorKind::ConnectionRefused, "p10kd did not answer after being started"))
}

enum Outcome {
    Attached,
    NoDaemon,
}

/// Runs in the runtime directory, where the socket is a relative name.
fn try_attach(pid: u32) -> io::Result<Outcome> {
    let mut stream = match UnixStream::connect(RuntimeDir::SOCKET_NAME) {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {
            return Ok(Outcome::NoDaemon)
        }
        Err(e) => return Err(e),
    };
    stream.write_all(format!("attach {pid}\n").as_bytes())?;
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer)?;
    match answer.trim_end() {
        "ok" => Ok(Outcome::Attached),
        "" => Ok(Outcome::NoDaemon),
        other => Err(io::Error::other(format!("p10kd refused attach: {other}"))),
    }
}

/// Runs `p10kd serve`, which returns once a daemon is listening (its own or another's).
fn spawn_serve(rt: &RuntimeDir) -> io::Result<()> {
    let status = Command::new(std::env::current_exe()?)
        .arg("serve")
        .arg(rt.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("p10kd serve exited with {status}")))
    }
}
