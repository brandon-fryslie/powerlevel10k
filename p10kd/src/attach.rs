//! The shell-side helper: `p10kd attach <base> <pid>` prints the runtime directory and
//! returns 0 once `<pid>.req` and `<pid>.resp` exist there and can be opened without
//! blocking. It starts the daemon when none is listening, and keeps trying for a bounded
//! time if the daemon it reached was shutting down (its answer is then end-of-file rather
//! than `ok`) or is between losing its socket and re-binding it (serve.rs).

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::runtime::RuntimeDir;

/// How long a listening daemon (`serve` reported one) may take to become connectable,
/// and how long a connected one may take to answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_RETRY: Duration = Duration::from_millis(50);

pub fn attach(rt: &RuntimeDir, pid: u32) -> io::Result<()> {
    rt.enter()?;
    if try_attach(pid)? == Outcome::NoDaemon {
        spawn_serve(rt)?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while try_attach(pid)? == Outcome::NoDaemon {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("p10kd did not answer within {CONNECT_TIMEOUT:?} of being started"),
                ));
            }
            std::thread::sleep(CONNECT_RETRY);
        }
    }
    let mut out = io::stdout().lock();
    out.write_all(rt.path().as_os_str().as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

#[derive(PartialEq)]
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
    stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    stream.write_all(format!("attach {pid}\n").as_bytes())?;
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            io::Error::new(io::ErrorKind::TimedOut, format!("p10kd is listening but did not answer within {CONNECT_TIMEOUT:?}"))
        }
        _ => e,
    })?;
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
        .arg(rt.base())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("p10kd serve exited with {status}")))
    }
}
