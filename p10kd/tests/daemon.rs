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

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("p10kd-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Shell {
    req: File,
    resp: File,
    buf: Vec<u8>,
}

impl Shell {
    /// What `_p9k_worker_start` does: attach, open both FIFOs with O_NONBLOCK, say hello.
    fn attach(dir: &Path, pid: u32) -> Shell {
        let status = Command::new(bin()).arg("attach").arg(dir).arg(pid.to_string()).status().unwrap();
        assert!(status.success(), "attach exited {status}");
        let req = OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(dir.join(format!("{pid}.req"))).unwrap();
        let resp = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(dir.join(format!("{pid}.resp"))).unwrap();
        let mut shell = Shell { req, resp, buf: Vec::new() };
        shell.send(b"h");
        shell
    }

    fn send(&mut self, record: &[u8]) {
        self.req.write_all(record).unwrap();
        self.req.write_all(b"\x1e").unwrap();
    }

    fn request(&mut self, fields: &[&str]) {
        self.send(fields.join("\x1f").as_bytes());
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
}

fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn attach_request_reply_detach_exit() {
    let dir = fresh_dir("basic");
    let mut shell = Shell::attach(&dir, 4242);
    assert!(dir.join("sock").exists());

    shell.request(&["load", "_p9k_prompt_load_sync", "load"]);
    let e = shell.recv(Duration::from_secs(2));
    assert!(e.starts_with("e_p9k_prompt_load_sync '"), "{e}");
    assert_eq!(e.matches('\'').count(), 6, "three quoted facts: {e}");
    assert_eq!(shell.recv(Duration::from_secs(2)), "dload");

    shell.request(&["disk_usage", "_p9k_prompt_disk_usage_sync", "disk_usage", "/"]);
    let e = shell.recv(Duration::from_secs(2));
    let pct: u8 = e.trim_start_matches("e_p9k_prompt_disk_usage_sync '").trim_end_matches('\'').parse().unwrap();
    assert!(pct <= 100);
    assert_eq!(shell.recv(Duration::from_secs(2)), "ddisk_usage");

    // A sampler that fails still finishes the request, so the shell's queue never sticks.
    shell.request(&["gcloud", "_p9k_prompt_gcloud_update 'c' 'a' 'p'", "gcloud", "/no/such/gcloud", "c", "a", "p"]);
    assert_eq!(shell.recv(Duration::from_secs(2)), "e_p9k_prompt_gcloud_update 'c' 'a' 'p' ''");
    assert_eq!(shell.recv(Duration::from_secs(2)), "dgcloud");

    // The time tick arrives at the next interval boundary.
    let t0 = Instant::now();
    shell.request(&["time", "_p9k_prompt_time_sync", "time", "0.2"]);
    assert_eq!(shell.recv(Duration::from_secs(2)), "e_p9k_prompt_time_sync");
    assert_eq!(shell.recv(Duration::from_secs(2)), "dtime");
    assert!(t0.elapsed() <= Duration::from_millis(400), "{:?}", t0.elapsed());

    // Closing the request FIFO is the detach; with no shells left the daemon exits.
    drop(shell);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    wait_until("fifo removal", Duration::from_secs(3), || !dir.join("4242.req").exists() && !dir.join("4242.resp").exists());
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert!(log.contains("attach 4242") && log.contains("detach 4242") && log.contains("last shell left"), "{log}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn two_shells_share_one_daemon_and_a_bad_request_only_drops_its_shell() {
    let dir = fresh_dir("shared");
    let mut a = Shell::attach(&dir, 1);
    let mut b = Shell::attach(&dir, 2);
    let log_pids = || {
        std::fs::read_to_string(dir.join("log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("started"))
            .count()
    };
    assert_eq!(log_pids(), 1, "exactly one daemon started");

    a.request(&["ram", "cb", "ram"]);
    assert!(a.recv(Duration::from_secs(2)).starts_with("ecb '"));
    assert_eq!(a.recv(Duration::from_secs(2)), "dram");

    // Malformed request: the daemon closes that shell's session, loudly in the log.
    b.send(b"nonsense");
    wait_until("b detached", Duration::from_secs(3), || {
        std::fs::read_to_string(dir.join("log")).unwrap_or_default().contains("detach 2: bad request")
    });

    // a is unaffected.
    a.request(&["swap", "cb", "swap"]);
    assert!(a.recv(Duration::from_secs(2)).starts_with("ecb '"));
    assert_eq!(a.recv(Duration::from_secs(2)), "dswap");

    drop(a);
    drop(b);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn concurrent_attaches_start_exactly_one_daemon() {
    let dir = fresh_dir("race");
    std::fs::create_dir_all(&dir).unwrap();
    let handles: Vec<_> = (10..18)
        .map(|pid| {
            let dir = dir.clone();
            std::thread::spawn(move || Shell::attach(&dir, pid))
        })
        .collect();
    let shells: Vec<Shell> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert_eq!(log.matches("started").count(), 1, "{log}");
    drop(shells);
    wait_until("socket removal", Duration::from_secs(3), || !dir.join("sock").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
