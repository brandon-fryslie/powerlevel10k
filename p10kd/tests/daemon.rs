//! End to end through the real binary: attach, FIFO handshake, requests, replies, and
//! the daemon leaving when its last shell does.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_p10kd"))
}

fn fresh_base(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("p10kd-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Escape one field the way `_p9k_worker_invoke` does.
fn escape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &b in field {
        match b {
            0x1d => out.extend_from_slice(b"\x1dD"),
            0x1e => out.extend_from_slice(b"\x1dE"),
            0x1f => out.extend_from_slice(b"\x1dF"),
            b => out.push(b),
        }
    }
    out
}

struct Shell {
    dir: PathBuf,
    req: File,
    resp: File,
    buf: Vec<u8>,
}

impl Shell {
    /// What `_p9k_worker_start` does: attach, open both FIFOs in the directory attach
    /// printed with O_NONBLOCK, say hello.
    fn attach(base: &Path, pid: u32) -> Shell {
        let out = Command::new(bin()).arg("attach").arg(base).arg(pid.to_string()).output().unwrap();
        assert!(out.status.success(), "attach exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr));
        let dir = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim_end());
        assert_eq!(dir.parent(), Some(base), "{}", dir.display());
        let req = OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(dir.join(format!("{pid}.req"))).unwrap();
        let resp = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(dir.join(format!("{pid}.resp"))).unwrap();
        let mut shell = Shell { dir, req, resp, buf: Vec::new() };
        shell.send(b"h");
        shell
    }

    fn send(&mut self, record: &[u8]) {
        self.req.write_all(record).unwrap();
        self.req.write_all(b"\x1e").unwrap();
    }

    fn request(&mut self, fields: &[&[u8]]) {
        let escaped: Vec<Vec<u8>> = fields.iter().map(|f| escape(f)).collect();
        self.send(&escaped.join(&0x1fu8));
    }

    /// Next complete record, waiting up to `timeout`.
    fn recv(&mut self, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(end) = self.buf.iter().position(|&b| b == 0x1e) {
                let rec = String::from_utf8(self.buf[..end].to_vec()).unwrap();
                self.buf.drain(..=end);
                return rec;
            }
            let mut chunk = [0u8; 4096];
            match self.resp.read(&mut chunk) {
                Ok(0) => panic!("resp fifo closed"),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no reply within {timeout:?}; buffered {:?}", self.buf);
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("log")).unwrap_or_default()
    }
}

fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn s(bytes: &str) -> &[u8] {
    bytes.as_bytes()
}

#[test]
fn attach_request_reply_detach_exit() {
    let base = fresh_base("basic");
    let mut shell = Shell::attach(&base, 4242);
    let dir = shell.dir.clone();
    assert!(dir.join("sock").exists());

    shell.request(&[s("load"), s("_p9k_prompt_load_sync"), s("load")]);
    let e = shell.recv(Duration::from_secs(2));
    assert!(e.starts_with("e_p9k_prompt_load_sync '"), "{e}");
    assert_eq!(e.matches('\'').count(), 6, "three quoted facts: {e}");
    assert_eq!(shell.recv(Duration::from_secs(2)), "dload");

    shell.request(&[s("disk_usage"), s("_p9k_prompt_disk_usage_sync"), s("disk_usage"), s("/")]);
    let e = shell.recv(Duration::from_secs(2));
    let pct: u8 = e.trim_start_matches("e_p9k_prompt_disk_usage_sync '").trim_end_matches('\'').parse().unwrap();
    assert!(pct <= 100);
    assert_eq!(shell.recv(Duration::from_secs(2)), "ddisk_usage");

    // A program that is missing is a fact (an empty one), not a failure.
    shell.request(&[s("gcloud"), s("_p9k_prompt_gcloud_update 'c' 'a' 'p'"), s("gcloud"), s(""), s("/no/such/gcloud"), s("c"), s("a"), s("p")]);
    assert_eq!(shell.recv(Duration::from_secs(2)), "e_p9k_prompt_gcloud_update 'c' 'a' 'p' ''");
    assert_eq!(shell.recv(Duration::from_secs(2)), "dgcloud");

    // A sampler that fails (statvfs ENOTDIR here) still runs the callback, with no
    // facts, and still finishes the request: the segment can re-arm and the queue moves.
    shell.request(&[s("disk_usage"), s("_p9k_prompt_disk_usage_sync"), s("disk_usage"), s("/etc/passwd/not-a-dir")]);
    assert_eq!(shell.recv(Duration::from_secs(2)), "e_p9k_prompt_disk_usage_sync");
    assert_eq!(shell.recv(Duration::from_secs(2)), "ddisk_usage");
    assert!(shell.log().contains("statvfs /etc/passwd/not-a-dir"), "{}", shell.log());

    // The time tick arrives at the next interval boundary.
    let t0 = Instant::now();
    shell.request(&[s("time"), s("_p9k_prompt_time_sync"), s("time"), s("0.2")]);
    assert_eq!(shell.recv(Duration::from_secs(2)), "e_p9k_prompt_time_sync");
    assert_eq!(shell.recv(Duration::from_secs(2)), "dtime");
    assert!(t0.elapsed() <= Duration::from_millis(400), "{:?}", t0.elapsed());

    // Closing the request FIFO is the detach; with no shells left the daemon exits.
    drop(shell);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    wait_until("fifo removal", Duration::from_secs(3), || !dir.join("4242.req").exists() && !dir.join("4242.resp").exists());
    let log = || std::fs::read_to_string(dir.join("log")).unwrap_or_default();
    wait_until("exit logged", Duration::from_secs(3), || log().contains("last shell left"));
    assert!(log().contains("attach 4242") && log().contains("detach 4242"), "{}", log());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn programs_run_in_the_environment_the_request_carries() {
    let base = fresh_base("env");
    let tools = base.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let curl = tools.join("curl");
    std::fs::write(&curl, "#!/bin/sh\necho \"$ANSWER\"\n").unwrap();
    std::fs::set_permissions(&curl, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let env = format!("PATH={}\0ANSWER=203.0.113.9", tools.display());

    let mut shell = Shell::attach(&base, 77);
    shell.request(&[s("public_ip"), s("cb"), s("public_ip"), env.as_bytes(), s("https://x.invalid/"), s("curl")]);
    assert_eq!(shell.recv(Duration::from_secs(2)), "ecb '203.0.113.9'");
    assert_eq!(shell.recv(Duration::from_secs(2)), "dpublic_ip");

    // With a PATH that holds no curl, no method answers.
    let empty = base.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let env = format!("PATH={}\0ANSWER=203.0.113.9", empty.display());
    shell.request(&[s("public_ip"), s("cb"), s("public_ip"), env.as_bytes(), s("https://x.invalid/"), s("curl")]);
    assert_eq!(shell.recv(Duration::from_secs(2)), "ecb ''");
    assert_eq!(shell.recv(Duration::from_secs(2)), "dpublic_ip");
    let dir = shell.dir.clone();
    drop(shell);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn separators_in_a_path_survive_the_wire() {
    let base = fresh_base("escape");
    let weird = base.join("we\x1dird\x1e\x1fdir");
    std::fs::create_dir_all(&weird).unwrap();
    let mut shell = Shell::attach(&base, 78);
    shell.request(&[s("disk_usage"), s("cb"), s("disk_usage"), weird.as_os_str().as_encoded_bytes()]);
    let e = shell.recv(Duration::from_secs(2));
    let pct: u8 = e.trim_start_matches("ecb '").trim_end_matches('\'').parse().unwrap_or_else(|_| panic!("{e}"));
    assert!(pct <= 100);
    assert_eq!(shell.recv(Duration::from_secs(2)), "ddisk_usage");
    let dir = shell.dir.clone();
    drop(shell);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn two_shells_share_one_daemon_and_a_bad_request_only_drops_its_shell() {
    let base = fresh_base("shared");
    let mut a = Shell::attach(&base, 1);
    let mut b = Shell::attach(&base, 2);
    let dir = a.dir.clone();
    assert_eq!(a.log().lines().filter(|l| l.contains("started")).count(), 1, "exactly one daemon started");

    a.request(&[s("ram"), s("cb"), s("ram")]);
    assert!(a.recv(Duration::from_secs(2)).starts_with("ecb '"));
    assert_eq!(a.recv(Duration::from_secs(2)), "dram");

    // Malformed request: the daemon closes that shell's session, loudly in the log.
    b.send(b"nonsense");
    wait_until("b detached", Duration::from_secs(3), || a.log().contains("detach 2: bad request"));

    // a is unaffected.
    a.request(&[s("swap"), s("cb"), s("swap")]);
    assert!(a.recv(Duration::from_secs(2)).starts_with("ecb '"));
    assert_eq!(a.recv(Duration::from_secs(2)), "dswap");

    drop(a);
    drop(b);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn concurrent_attaches_start_exactly_one_daemon() {
    let base = fresh_base("race");
    let handles: Vec<_> = (10..18)
        .map(|pid| {
            let base = base.clone();
            std::thread::spawn(move || Shell::attach(&base, pid))
        })
        .collect();
    let shells: Vec<Shell> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let dir = shells[0].dir.clone();
    let log = shells[0].log();
    assert_eq!(log.matches("started").count(), 1, "{log}");
    drop(shells);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_lagging_old_session_does_not_remove_the_fifos_of_a_reattached_shell() {
    // What `p10k reload` does: stop the worker, start it again, same pid. Here the old
    // session is still open when the new one attaches, so its thread ends only after the
    // new FIFOs exist at the same paths, and must leave them alone.
    let base = fresh_base("reattach");
    let first = Shell::attach(&base, 501);
    let mut second = Shell::attach(&base, 501);
    let dir = second.dir.clone();
    drop(first);
    wait_until("first session to end", Duration::from_secs(3), || second.log().matches("detach 501").count() == 1);
    assert!(dir.join("501.req").exists() && dir.join("501.resp").exists(), "the new session's fifos were removed");
    second.request(&[s("load"), s("cb"), s("load")]);
    assert!(second.recv(Duration::from_secs(2)).starts_with("ecb '"));
    assert_eq!(second.recv(Duration::from_secs(2)), "dload");
    drop(second);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    wait_until("fifo removal", Duration::from_secs(3), || !dir.join("501.req").exists());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_removed_socket_is_rebound_and_new_shells_still_attach() {
    // A temporary-directory sweeper removes the socket from under a living daemon.
    let base = fresh_base("sweep");
    let mut a = Shell::attach(&base, 600);
    let dir = a.dir.clone();
    std::fs::remove_file(dir.join("sock")).unwrap();
    let mut b = Shell::attach(&base, 601);
    assert_eq!(a.log().matches("started").count(), 1, "the living daemon served b: {}", a.log());
    assert!(a.log().contains("re-bound"), "{}", a.log());
    b.request(&[s("load"), s("cb"), s("load")]);
    assert!(b.recv(Duration::from_secs(2)).starts_with("ecb '"));
    a.request(&[s("load"), s("cb"), s("load")]);
    assert!(a.recv(Duration::from_secs(2)).starts_with("ecb '"));
    drop(a);
    drop(b);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    std::fs::remove_dir_all(&base).unwrap();
}
