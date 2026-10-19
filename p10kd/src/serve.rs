//! The daemon: one process per user that every shell attaches to.
//!
//! Lifecycle, with its one explicit owner ([LAW:no-ambient-temporal-coupling]):
//!
//! 1. `serve` takes `start.lock`, then tries `run.lock`. Losing `run.lock` means a daemon
//!    is already listening (the holder only releases `start.lock` once it listens), so
//!    `serve` exits 0 and the caller connects to it.
//! 2. The winner binds the socket, forks, and the parent exits 0 only after the child is
//!    listening: a caller that spawned `serve` can connect the moment it returns.
//! 3. A shell attaches over the socket; the daemon creates that shell's two FIFOs and
//!    serves it from a thread until the shell closes its request FIFO.
//! 4. When the last shell leaves, the daemon takes `start.lock`, checks nobody attached
//!    meanwhile, removes its socket and exits, releasing `run.lock` before `start.lock`
//!    so a starter that was waiting never sees "running" without a listener.
//!
//! The socket is the daemon's to keep: temporary-directory sweepers remove entries that
//! look idle (macOS after three days, systemd-tmpfiles after ten), so the accept loop
//! checks once a second that its socket is still at its path and re-binds when it is
//! gone. When even that fails (the directory itself was swept) it serves the shells it
//! has and accepts no more. On exit it removes the socket only if it is still its own.
//!
//! Each shell's traffic rides FIFOs rather than the socket because a zsh `zsocket`
//! descriptor is inherited by every child the shell spawns, while `sysopen -o cloexec`
//! on a FIFO is not.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::lock::FileLock;
use crate::log;
use crate::protocol::{self, Facts, RecordBuffer, Request, HELLO};
use crate::runtime::RuntimeDir;
use crate::sample;

/// How long an attached shell has to open its FIFOs and say hello before the daemon
/// concludes it died in between and reclaims the FIFOs.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the accept loop checks that its socket is still on disk.
const SOCKET_CHECK: Duration = Duration::from_secs(1);

/// How long a peer that connected to the socket has to send its attach line.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(3);

pub fn serve(rt: &RuntimeDir) -> io::Result<()> {
    let start = FileLock::acquire(&rt.start_lock())?;
    let Some(run) = FileLock::try_acquire(&rt.run_lock())? else {
        return Ok(());
    };
    rt.enter()?;
    remove_if_exists(Path::new(RuntimeDir::SOCKET_NAME))?;
    let socket = Socket::bind()?;
    let logfile = OpenOptions::new().create(true).append(true).mode(0o600).open(rt.log())?;
    detach(&logfile)?;
    drop(logfile);
    log!("started, serving {}", rt.path().display());
    drop(start);
    accept_loop(rt, socket, run)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Becomes a session leader with stdio pointed at /dev/null and the log; the original
/// process exits 0 so a caller waiting on it knows the listener exists. The daemon keeps
/// the runtime directory as its cwd: the socket is a relative name there, and re-binding
/// needs it to stay so.
fn detach(logfile: &File) -> io::Result<()> {
    // SAFETY: no threads exist yet, so fork is safe; the parent only _exits.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid > 0 {
        // SAFETY: exiting the parent without running destructors is the point: its copies
        // of the listener and locks must not be closed or unlinked.
        unsafe { libc::_exit(0) };
    }
    // SAFETY: plain POSIX calls on descriptors we own.
    unsafe {
        if libc::setsid() < 0 {
            return Err(io::Error::last_os_error());
        }
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        let devnull = File::open("/dev/null")?;
        if libc::dup2(devnull.as_raw_fd(), 0) < 0
            || libc::dup2(logfile.as_raw_fd(), 1) < 0
            || libc::dup2(logfile.as_raw_fd(), 2) < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The listening socket together with the inode it was bound at, which is how the daemon
/// tells its own socket from a successor's at the same path. Once a re-bind has failed
/// the path is `abandoned`: the directory itself is gone, every later attempt would fail
/// the same way, and the daemon serves the shells it has until they leave.
struct Socket {
    listener: UnixListener,
    ino: u64,
    abandoned: bool,
}

impl Socket {
    /// Binds at [`RuntimeDir::SOCKET_NAME`] in the current directory.
    fn bind() -> io::Result<Socket> {
        let listener = UnixListener::bind(RuntimeDir::SOCKET_NAME)?;
        let ino = fs::symlink_metadata(RuntimeDir::SOCKET_NAME)?.ino();
        Ok(Socket { listener, ino, abandoned: false })
    }

    /// Whether the path still holds this socket.
    fn on_disk(&self) -> io::Result<bool> {
        match fs::symlink_metadata(RuntimeDir::SOCKET_NAME) {
            Ok(meta) => Ok(meta.ino() == self.ino),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Re-binds when the path is empty. A path that holds someone else's socket (possible
    /// only after a sweeper also removed the locks) is left to them. A re-bind that fails
    /// is logged once and not retried.
    fn rebind_if_gone(&mut self) {
        if self.abandoned {
            return;
        }
        let gone = matches!(fs::symlink_metadata(RuntimeDir::SOCKET_NAME), Err(e) if e.kind() == io::ErrorKind::NotFound);
        if !gone {
            return;
        }
        match Socket::bind() {
            Ok(socket) => {
                *self = socket;
                log!("socket was removed from under us; re-bound");
            }
            Err(e) => {
                self.abandoned = true;
                log!("socket was removed from under us and cannot be re-bound ({e}); serving attached shells only");
            }
        }
    }

    fn remove_if_ours(&self) -> io::Result<()> {
        if self.on_disk()? {
            fs::remove_file(RuntimeDir::SOCKET_NAME)?;
        }
        Ok(())
    }
}

fn accept_loop(rt: &RuntimeDir, mut socket: Socket, run: FileLock) -> io::Result<()> {
    let clients = Arc::new(AtomicUsize::new(0));
    let (notify_r, notify_w) = pipe()?;
    let notify_w = Arc::new(Mutex::new(notify_w));
    loop {
        let mut fds = [
            libc::pollfd { fd: socket.listener.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: notify_r.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: `fds` is a valid array of 2 pollfd.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, SOCKET_CHECK.as_millis() as libc::c_int) } < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if fds[0].revents != 0 {
            let (stream, _) = socket.listener.accept()?;
            attach(rt, stream, &clients, &notify_w);
        }
        // After the accept: a rebind replaces the listener poll reported on.
        socket.rebind_if_gone();
        if fds[1].revents != 0 {
            let mut drain = [0u8; 64];
            let _ = (&notify_r).read(&mut drain);
            if clients.load(Ordering::SeqCst) == 0 {
                let start = FileLock::acquire(&rt.start_lock())?;
                socket.listener.set_nonblocking(true)?;
                match socket.listener.accept() {
                    Ok((stream, _)) => {
                        socket.listener.set_nonblocking(false)?;
                        attach(rt, stream, &clients, &notify_w);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        socket.remove_if_ours()?;
                        log!("last shell left, exiting");
                        drop(run);
                        drop(start);
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

/// Handles one `attach <pid>` conversation on the socket: creates the shell's FIFOs,
/// makes them openable without blocking, and only then answers `ok`.
fn attach(rt: &RuntimeDir, mut stream: UnixStream, clients: &Arc<AtomicUsize>, notify: &Arc<Mutex<File>>) {
    let mut line = String::new();
    // A peer that connects and says nothing must not hold the accept loop.
    let read = stream.set_read_timeout(Some(ATTACH_TIMEOUT)).and_then(|()| BufReader::new(&stream).read_line(&mut line));
    if let Err(e) = read {
        log!("attach: reading request: {e}");
        return;
    }
    let pid = match line.trim().strip_prefix("attach ").and_then(|p| p.parse::<u32>().ok()) {
        Some(pid) => pid,
        None => {
            log!("attach: malformed request {line:?}");
            let _ = stream.write_all(b"error malformed attach request\n");
            return;
        }
    };
    match Fifos::create(rt, pid) {
        Ok(fifos) => {
            clients.fetch_add(1, Ordering::SeqCst);
            if let Err(e) = stream.write_all(b"ok\n") {
                log!("attach {pid}: answering: {e}");
            }
            log!("attach {pid}");
            let clients = Arc::clone(clients);
            let notify = Arc::clone(notify);
            thread::spawn(move || {
                let result = serve_client(pid, fifos);
                match result {
                    Ok(()) => log!("detach {pid}"),
                    Err(e) => log!("detach {pid}: {e}"),
                }
                clients.fetch_sub(1, Ordering::SeqCst);
                let _ = notify.lock().map(|mut w| w.write_all(b"x"));
            });
        }
        Err(e) => {
            log!("attach {pid}: creating fifos: {e}");
            let _ = stream.write_all(format!("error {e}\n").as_bytes());
        }
    }
}

/// One FIFO of one shell: the path it was created at and the inode it got, so that
/// removal on drop only ever removes this FIFO and never a successor created at the
/// same path for the same pid (a shell that re-attaches after `p10k reload`).
struct Fifo {
    path: PathBuf,
    ino: u64,
}

impl Fifo {
    fn create(path: PathBuf) -> io::Result<Fifo> {
        remove_if_exists(&path)?;
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
        // SAFETY: `c` is a valid NUL-terminated path.
        if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let ino = fs::symlink_metadata(&path)?.ino();
        Ok(Fifo { path, ino })
    }
}

impl Drop for Fifo {
    fn drop(&mut self) {
        if let Ok(meta) = fs::symlink_metadata(&self.path) {
            if meta.ino() == self.ino {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

/// The two FIFOs of one shell. `req_placeholder` is a read-write handle that exists so
/// the shell's write-open succeeds immediately; it is dropped once the shell has said
/// hello, so that from then on end-of-file on `req` means the shell is gone.
struct Fifos {
    req: File,
    req_placeholder: Option<File>,
    resp: Arc<Mutex<File>>,
    _req_fifo: Fifo,
    _resp_fifo: Fifo,
}

impl Fifos {
    fn create(rt: &RuntimeDir, pid: u32) -> io::Result<Fifos> {
        let req_fifo = Fifo::create(rt.req_fifo(pid))?;
        let resp_fifo = Fifo::create(rt.resp_fifo(pid))?;
        let req_placeholder = OpenOptions::new().read(true).write(true).open(&req_fifo.path)?;
        let req = OpenOptions::new().read(true).open(&req_fifo.path)?;
        let resp = OpenOptions::new().read(true).write(true).open(&resp_fifo.path)?;
        Ok(Fifos {
            req,
            req_placeholder: Some(req_placeholder),
            resp: Arc::new(Mutex::new(resp)),
            _req_fifo: req_fifo,
            _resp_fifo: resp_fifo,
        })
    }
}

fn serve_client(pid: u32, mut fifos: Fifos) -> io::Result<()> {
    wait_readable(&fifos.req, HELLO_TIMEOUT)
        .map_err(|e| io::Error::new(e.kind(), format!("waiting for hello from {pid}: {e}")))?;
    let mut buf = RecordBuffer::new();
    let mut chunk = [0u8; 8192];
    let mut greeted = false;
    loop {
        let n = fifos.req.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.push(&chunk[..n]);
        while let Some(record) = buf.next_record() {
            if !greeted {
                if record != HELLO {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("expected hello, got {record:?}")));
                }
                greeted = true;
                fifos.req_placeholder.take();
                continue;
            }
            let req = match Request::parse(&record) {
                Ok(req) => req,
                Err(e) => {
                    let shown = String::from_utf8_lossy(&record);
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad request {shown:?}: {e}")));
                }
            };
            let resp = Arc::clone(&fifos.resp);
            thread::spawn(move || {
                let id = String::from_utf8_lossy(&req.id).into_owned();
                // [LAW:dataflow-not-control-flow] the callback runs once per request whatever
                // the sampler did: a failure is logged here and reaches the shell as a reply
                // with no facts, so the segment's own state machine (public_ip's in-flight
                // mark, time's next tick) keeps turning instead of waiting forever.
                let facts = sample::sample(&req.segment).unwrap_or_else(|e| {
                    log!("{pid} {id}: {e}");
                    Facts::default()
                });
                let reply = protocol::render_reply(&req, &facts);
                if let Ok(mut w) = resp.lock() {
                    if let Err(e) = w.write_all(&reply) {
                        log!("{pid} {id}: writing reply: {e}");
                    }
                }
            });
        }
    }
}

fn wait_readable(file: &File, timeout: Duration) -> io::Result<()> {
    let mut fds = [libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
    // SAFETY: `fds` is one valid pollfd.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout.as_millis() as libc::c_int) };
    match rc {
        1 => Ok(()),
        0 => Err(io::Error::new(io::ErrorKind::TimedOut, format!("nothing arrived within {timeout:?}"))),
        _ => Err(io::Error::last_os_error()),
    }
}

fn pipe() -> io::Result<(File, File)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created by pipe and are owned by nothing else.
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}
