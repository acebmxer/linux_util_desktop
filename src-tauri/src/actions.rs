//! Install, uninstall and update runs.
//!
//! `linux_util.sh` refuses to run as root and calls `sudo` itself, so it runs
//! as the normal user with a `sudo` shim first on `PATH` that adds `-A`.
//! `SUDO_ASKPASS` points at this same executable started with `--askpass`; it
//! asks the running app for the password over a private Unix socket and the
//! app shows its own dialog. The password is checked once, kept in memory for
//! the length of one run and never written to disk.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

const OPS: [&str; 3] = ["install", "uninstall", "update"];
const SOCKET_ENV: &str = "LU_ASKPASS_SOCK";

#[derive(Default)]
pub struct RunState {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    running: bool,
    cancelled: bool,
    pid: Option<u32>,
    /// Set while the password dialog is open; `None` means the user cancelled.
    reply: Option<mpsc::Sender<Option<String>>>,
    /// The master side of the script's terminal, for answers typed into the run panel.
    stdin: Option<File>,
    /// Output received after the last newline, per stream; see `pump`.
    pending: Vec<Arc<Mutex<Vec<u8>>>>,
    /// The sudo password the last run was given, kept in memory only so the
    /// reboot offer after it does not ask again. Dropped when that offer ends.
    sudo_password: Option<String>,
}

#[derive(Serialize, Clone)]
struct Output<'a> {
    text: &'a str,
    stderr: bool,
    /// Shown in the error colour. The script's own stderr is not: tools such as
    /// dnf write their normal progress there, and a terminal shows it plain.
    error: bool,
    /// The line so far, still waiting for its newline (e.g. a `[y/N]` prompt).
    /// The next event on the same stream replaces it.
    partial: bool,
}

#[derive(Serialize, Clone)]
struct ItemEvent<'a> {
    index: usize,
    name: &'a str,
    /// "running", "ok", "failed" or "cancelled"
    state: &'a str,
    code: Option<i32>,
}

#[derive(Serialize, Clone)]
struct Done {
    ok: usize,
    failed: usize,
    cancelled: bool,
}

#[derive(Serialize, Clone)]
struct AskpassRequest {
    error: Option<String>,
}

/// Client side of `SUDO_ASKPASS`: print the password the app collected, or exit 1.
pub fn askpass_client() -> ! {
    let code = (|| {
        let path = std::env::var(SOCKET_ENV).ok()?;
        let mut stream = UnixStream::connect(path).ok()?;
        let mut password = String::new();
        stream.read_to_string(&mut password).ok()?;
        if password.is_empty() {
            return None;
        }
        print!("{password}");
        std::io::stdout().flush().ok()?;
        Some(())
    })();
    std::process::exit(if code.is_some() { 0 } else { 1 });
}

pub(crate) fn find_in_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&crate::host_var("PATH")?)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `sudo` the script runs. Restarting or stopping xrdp ends every open RDP
/// session, and this app (and the run) may be inside one, so while someone is
/// connected that one command is skipped with a notice. Everything else goes
/// to the real sudo unchanged.
const SUDO_SHIM: &str = r#"#!/bin/sh
if [ "$1" = systemctl ]; then
    case "$2" in
        restart|stop|try-restart|reload-or-restart|try-reload-or-restart)
            for unit in "$@"; do
                case "$unit" in
                    xrdp|xrdp.service|xrdp-sesman|xrdp-sesman.service)
                        if grep -lqx xrdp-sesexec /proc/[0-9]*/comm 2>/dev/null; then
                            echo "[WARN] Skipped 'systemctl $2 $unit': RDP sessions are open and would be ended. Restart it later, after logging out." >&2
                            exit 0
                        fi
                        ;;
                esac
            done
            ;;
    esac
fi
# sudo drops BASH_ENV, so a script run as `sudo bash <script>` (the guest tools
# ISO's installer) would lose the prompt-showing `read`; load it inside root's bash.
if [ "$1" = bash ]; then
    shift
    exec {REAL_SUDO} -A bash -c '. {PRELUDE}; export -f read; exec bash "$@"' bash "$@"
fi
exec {REAL_SUDO} -A "$@"
"#;

/// Sourced by every bash the script starts (`BASH_ENV`). Bash prints the prompt
/// of `read -p` only when stdin is a terminal, and here it is a pipe, so a
/// prompt such as "Retry? [y/N]" never showed and the run seemed stuck. This
/// prints it to stderr instead. Handles `-p` as its own option or last in a
/// cluster (`-rp`), which is how linux_util writes them.
const READ_PRELUDE: &str = r#"read() {
    if [ -p /dev/stdin ]; then
        local __rp_args=() __rp_prompt="" __rp_have=0 __rp_a
        while [ $# -gt 0 ]; do
            __rp_a=$1; shift
            case "$__rp_a" in
                -p) __rp_prompt=$1; __rp_have=1; shift ;;
                -[!-]*p) __rp_prompt=$1; __rp_have=1; shift; [ "${__rp_a%p}" != "-" ] && __rp_args+=("${__rp_a%p}") ;;
                *) __rp_args+=("$__rp_a") ;;
            esac
        done
        [ "$__rp_have" = 1 ] && printf '%s' "$__rp_prompt" >&2
        builtin read "${__rp_args[@]}"
        return
    fi
    builtin read "$@"
}
"#;

/// A pseudo-terminal for one script. linux_util's installers ask with
/// `read ... < /dev/tty`, which only exists when the process has a controlling
/// terminal; the app has none, so those prompts failed and the install was
/// cancelled. The script gets the slave as stdin and controlling terminal, and
/// answers typed in the run panel are written to the master.
struct Pty {
    master: File,
    slave: File,
}

impl Pty {
    fn open() -> std::io::Result<Self> {
        let (mut m, mut sl) = (0, 0);
        let rc = unsafe {
            libc::openpty(&mut m, &mut sl, std::ptr::null_mut(), std::ptr::null(), std::ptr::null())
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let (master, slave) = unsafe { (File::from_raw_fd(m), File::from_raw_fd(sl)) };
        // The panel echoes typed answers itself, so the terminal must not.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut t) } == 0 {
            t.c_lflag &= !(libc::ECHO | libc::ECHONL);
            unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &t) };
        }
        for f in [&master, &slave] {
            unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        Ok(Pty { master, slave })
    }
}

/// Per-run scratch directory (mode 0700) holding the shim, askpass and socket.
struct Workdir {
    root: PathBuf,
}

impl Workdir {
    fn create(real_sudo: &Path) -> Result<Self, String> {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("linux-util-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |p: &Path| {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(p)
                .map_err(|e| format!("Could not create {}: {e}", p.display()))
        };
        mk(&root)?;
        let work = Workdir { root };
        mk(&work.bin_dir())?;

        let exe = std::env::current_exe().map_err(|e| format!("Cannot find own executable: {e}"))?;
        let write_exec = |p: PathBuf, body: String| -> Result<(), String> {
            std::fs::write(&p, body).map_err(|e| format!("Could not write {}: {e}", p.display()))?;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())
        };
        write_exec(
            work.bin_dir().join("sudo"),
            SUDO_SHIM
                .replace("{REAL_SUDO}", &shell_quote(&real_sudo.to_string_lossy()))
                .replace("{PRELUDE}", &shell_quote(&work.prelude_path().to_string_lossy())),
        )?;
        // linux_util.sh runs with the AppImage's library path removed (see
        // `use_host_env`), but this executable still needs the bundled libraries.
        let libs = match (crate::host_var("LD_LIBRARY_PATH"), std::env::var("LD_LIBRARY_PATH")) {
            (host, Ok(own)) if host.as_deref() != Some(std::ffi::OsStr::new(&own)) => {
                format!("LD_LIBRARY_PATH={}\nexport LD_LIBRARY_PATH\n", shell_quote(&own))
            }
            _ => String::new(),
        };
        write_exec(
            work.askpass_path(),
            format!("#!/bin/sh\n{libs}exec {} --askpass \"$@\"\n", shell_quote(&exe.to_string_lossy())),
        )?;
        std::fs::write(work.prelude_path(), READ_PRELUDE)
            .map_err(|e| format!("Could not write {}: {e}", work.prelude_path().display()))?;
        Ok(work)
    }
    fn prelude_path(&self) -> PathBuf {
        self.root.join("prelude.sh")
    }
    fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }
    fn askpass_path(&self) -> PathBuf {
        self.root.join("askpass")
    }
    fn socket_path(&self) -> PathBuf {
        self.root.join("s")
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// True when `password` is accepted by sudo. Ignores any cached credentials.
fn password_ok(real_sudo: &Path, password: &str) -> bool {
    let mut cmd = Command::new(real_sudo);
    crate::use_host_env(&mut cmd);
    let Ok(mut child) = cmd
        .args(["-S", "-k", "-v", "-p", ""])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{password}");
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[derive(Default)]
struct Credentials {
    password: Option<String>,
    denied: bool,
}

fn serve_askpass(
    app: AppHandle,
    listener: UnixListener,
    real_sudo: PathBuf,
    stop: Arc<AtomicBool>,
) -> Option<String> {
    let mut creds = Credentials::default();
    let _ = listener.set_nonblocking(true);
    while !stop.load(Ordering::SeqCst) {
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let _ = stream.set_nonblocking(false);
        if creds.denied {
            continue; // dropping the stream makes askpass exit 1
        }
        if creds.password.is_none() {
            let state = app.state::<RunState>();
            let mut error = None;
            loop {
                let (tx, rx) = mpsc::channel();
                state.inner.lock().unwrap().reply = Some(tx);
                let _ = app.emit("askpass-request", AskpassRequest { error: error.take() });
                match rx.recv() {
                    Ok(Some(pw)) if password_ok(&real_sudo, &pw) => {
                        creds.password = Some(pw);
                        break;
                    }
                    Ok(Some(_)) => error = Some("Wrong password, try again.".to_string()),
                    _ => {
                        creds.denied = true;
                        break;
                    }
                }
            }
            state.inner.lock().unwrap().reply = None;
            let _ = app.emit("askpass-close", ());
        }
        if let Some(pw) = &creds.password {
            let _ = stream.write_all(pw.as_bytes());
        }
    }
    creds.password
}

/// Copies `from` into `to`, leaving out top-level entries named in `skip`.
fn copy_tree(from: &Path, to: &Path, skip: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if skip.iter().any(|s| entry.file_name() == *s) {
            continue;
        }
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            copy_tree(&src, &dst, &[])?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Where actions run linux_util from; its `logs/` directory is inside.
pub(crate) fn staged_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| format!("No app data directory: {e}"))?
        .join("linux_util"))
}

/// `linux_util.sh` writes `logs/` next to itself, and the bundled copy may sit
/// in a read-only directory. Actions therefore run from a per-user copy in the
/// app data directory, refreshed when the bundled version changes. `logs/` is kept.
fn staged_script(app: &AppHandle, bundled: &Path) -> Result<PathBuf, String> {
    let src = bundled.parent().ok_or("Bundled linux_util.sh has no parent directory")?;
    let dest = staged_dir(app)?;
    // The size of the script is part of the stamp so an edited bundled script
    // is re-staged even when the version number has not moved.
    let size = std::fs::metadata(src.join("linux_util.sh")).map(|m| m.len()).unwrap_or(0);
    let version = format!(
        "{} {size}",
        std::fs::read_to_string(src.join("linux_util.VERSION")).unwrap_or_default().trim()
    );
    let marker = dest.join(".staged-version");
    let current = std::fs::read_to_string(&marker).ok();
    if current.as_deref() != Some(version.trim()) || !dest.join("linux_util.sh").is_file() {
        let err = |e: std::io::Error| format!("Could not stage linux_util in {}: {e}", dest.display());
        if let Ok(entries) = std::fs::read_dir(&dest) {
            for entry in entries.flatten() {
                if entry.file_name() == "logs" {
                    continue;
                }
                let path = entry.path();
                let _ = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
            }
        }
        // A source checkout's vendor/ collects logs from `--list` runs; they are
        // not this copy's logs.
        copy_tree(src, &dest, &["logs"]).map_err(err)?;
        std::fs::write(&marker, version.trim()).map_err(err)?;
    }
    Ok(dest.join("linux_util.sh"))
}

/// Drops ANSI escape sequences and keeps only what a carriage return left last.
pub(crate) fn clean_line(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let text = text.trim_end_matches(['\n', '\r']);
    let text = text.rsplit('\r').next().unwrap_or("");
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// linux_util's `System Updates` ends with this line when nothing changed. It
/// returns 3 ("success, no changes"), but `--install` turns every non-zero
/// code into exit 1, so the line is how the app tells that case from a failure.
const NO_CHANGES_LINE: &str = "No package changes were made.";

/// How long output is still read after the script itself has exited. A process
/// it left running in the background keeps the pipes and terminal open for as
/// long as it lives, and the item must not stay "running" until then: in a
/// terminal, linux_util's menu carries on in the same case.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);

/// Sends `reader`'s output to the UI a line at a time. Text left after the
/// last newline is also sent, as a partial line, so a prompt that waits for an
/// answer without ending its line is visible before the answer is typed.
fn pump<R: Read + Send + 'static>(
    app: AppHandle,
    mut reader: R,
    stderr: bool,
    pending: Arc<Mutex<Vec<u8>>>,
    no_changes: Arc<AtomicBool>,
    done: mpsc::Sender<()>,
    detached: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let _done = done;
        let mut chunk = [0u8; 4096];
        loop {
            let n = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            // Past the grace period the item has ended; keep reading so the
            // leftover process never blocks on a full pipe, but show nothing.
            if detached.load(Ordering::SeqCst) {
                continue;
            }
            let mut pending = pending.lock().unwrap();
            pending.extend_from_slice(&chunk[..n]);
            while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                let text = clean_line(&line);
                if text.contains(NO_CHANGES_LINE) {
                    no_changes.store(true, Ordering::SeqCst);
                }
                let _ = app.emit("run-output", Output { text: &text, stderr, error: false, partial: false });
            }
            if !pending.is_empty() {
                let text = clean_line(&pending);
                let _ = app.emit("run-output", Output { text: &text, stderr, error: false, partial: true });
            }
        }
        let mut pending = pending.lock().unwrap();
        if !pending.is_empty() && !detached.load(Ordering::SeqCst) {
            let text = clean_line(&pending);
            pending.clear();
            let _ = app.emit("run-output", Output { text: &text, stderr, error: false, partial: false });
        }
    });
}

/// Waits until every output reader has ended or `grace` has passed, and says
/// which. Each reader holds a sender, so the channel disconnects once all end.
fn wait_for_readers(done: &mpsc::Receiver<()>, grace: Duration) -> bool {
    matches!(done.recv_timeout(grace), Err(mpsc::RecvTimeoutError::Disconnected))
}

/// Tells the UI a line of output. `stderr` lines show in the error colour.
pub(crate) fn emit_output(app: &AppHandle, text: &str, stderr: bool) {
    let _ = app.emit("run-output", Output { text, stderr, error: stderr, partial: false });
}

/// Tells the UI the state of queue entry `index`.
pub(crate) fn emit_item(app: &AppHandle, index: usize, name: &str, state: &str, code: Option<i32>) {
    let _ = app.emit("run-item", ItemEvent { index, name, state, code });
}

/// Ends the run in the UI.
pub(crate) fn emit_done(app: &AppHandle, ok: usize, failed: usize, cancelled: bool) {
    let _ = app.emit("run-done", Done { ok, failed, cancelled });
}

/// True once the user pressed Cancel during the current run.
pub(crate) fn is_cancelled(app: &AppHandle) -> bool {
    app.state::<RunState>().inner.lock().unwrap().cancelled
}

/// Runs the commands `make` builds, one per name, with a `sudo` that asks the
/// password through the app's own dialog. Emits the run events, including the
/// final `run-done`. `make` returns the command before the environment for
/// the shim is added.
pub(crate) fn run_queue(app: &AppHandle, names: &[String], make: &dyn Fn(&str) -> Command) {
    let state = app.state::<RunState>();
    let (mut ok, mut failed) = (0, 0);

    let real_sudo = find_in_path("sudo");
    let setup = real_sudo
        .ok_or_else(|| "sudo was not found on PATH".to_string())
        .and_then(|sudo| Workdir::create(&sudo).map(|w| (sudo, w)));
    let (real_sudo, work) = match setup {
        Ok(v) => v,
        Err(e) => {
            emit_output(app, &e, true);
            emit_done(app, 0, names.len(), false);
            return;
        }
    };
    state.inner.lock().unwrap().sudo_password = None;
    let stop = Arc::new(AtomicBool::new(false));
    let server = UnixListener::bind(work.socket_path()).ok().map(|listener| {
        let (app, stop, sudo) = (app.clone(), stop.clone(), real_sudo.clone());
        std::thread::spawn(move || serve_askpass(app, listener, sudo, stop))
    });

    let mut path = std::ffi::OsString::from(work.bin_dir());
    if let Some(old) = crate::host_var("PATH") {
        path.push(":");
        path.push(old);
    }

    for (index, name) in names.iter().enumerate() {
        if state.inner.lock().unwrap().cancelled {
            let _ = app.emit("run-item", ItemEvent { index, name, state: "cancelled", code: None });
            continue;
        }
        let _ = app.emit("run-item", ItemEvent { index, name, state: "running", code: None });
        let mut cmd = make(name);
        let pty = Pty::open().ok();
        cmd.env("PATH", &path)
            .env("SUDO_ASKPASS", work.askpass_path())
            .env(SOCKET_ENV, work.socket_path())
            .env("BASH_ENV", work.prelude_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match pty.as_ref().and_then(|p| p.slave.try_clone().ok()) {
            Some(slave) => {
                cmd.stdin(Stdio::from(slave));
                // New session whose controlling terminal is the pty (fd 0, set
                // up before this runs); its process group is then the child's pid.
                unsafe {
                    cmd.pre_exec(|| {
                        if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            None => {
                cmd.stdin(Stdio::piped()).process_group(0);
            }
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("Could not start {}: {e}", cmd.get_program().to_string_lossy());
                let _ = app.emit("run-output", Output { text: &msg, stderr: true, error: true, partial: false });
                let _ = app.emit("run-item", ItemEvent { index, name, state: "failed", code: None });
                failed += 1;
                continue;
            }
        };
        let pending = [Arc::default(), Arc::default()];
        {
            let mut inner = state.inner.lock().unwrap();
            inner.pid = Some(child.id());
            inner.stdin = match &pty {
                Some(p) => p.master.try_clone().ok(),
                None => None,
            }
            .or_else(|| {
                // No pty: the pipe's write end, wrapped as a File.
                child.stdin.take().map(|s| unsafe { File::from_raw_fd(s.into_raw_fd()) })
            });
            inner.pending = pending.to_vec();
        }
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let detached = Arc::new(AtomicBool::new(false));
        // Drain what the script writes to its terminal (nothing normally) so it
        // never blocks; reading ends with EIO once the script and its children exit.
        if let Some(Pty { master, slave }) = pty {
            drop(slave);
            let done = done_tx.clone();
            std::thread::spawn(move || {
                let _done = done;
                let mut master = master;
                let mut buf = [0u8; 1024];
                while matches!(master.read(&mut buf), Ok(n) if n > 0) {}
            });
        }
        let [out_pending, err_pending] = pending;
        let no_changes = Arc::new(AtomicBool::new(false));
        pump(app.clone(), child.stdout.take().unwrap(), false, out_pending, no_changes.clone(), done_tx.clone(), detached.clone());
        pump(app.clone(), child.stderr.take().unwrap(), true, err_pending, no_changes.clone(), done_tx.clone(), detached.clone());
        drop(done_tx);
        let status = child.wait();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.stdin = None; // closing it lets a child left reading stdin see EOF
            inner.pending.clear();
        }
        // Readers still going after this belong to a background process the
        // script left behind; they carry on, detached, until it closes them.
        wait_for_readers(&done_rx, OUTPUT_GRACE);
        detached.store(true, Ordering::SeqCst);
        let cancelled = {
            let mut inner = state.inner.lock().unwrap();
            inner.pid = None;
            inner.cancelled
        };
        let mut code = status.as_ref().ok().and_then(|s| s.code());
        if code == Some(1) && no_changes.load(Ordering::SeqCst) {
            code = Some(3);
        }
        let item_state = if cancelled {
            "cancelled"
        } else if matches!(code, Some(0 | 3)) {
            ok += 1;
            "ok"
        } else {
            failed += 1;
            "failed"
        };
        let _ = app.emit("run-item", ItemEvent { index, name, state: item_state, code });
    }

    stop.store(true, Ordering::SeqCst);
    if let Some(Ok(password)) = server.map(|s| s.join()) {
        state.inner.lock().unwrap().sudo_password = password;
    }
    let cancelled = state.inner.lock().unwrap().cancelled;
    let _ = app.emit("run-done", Done { ok, failed, cancelled });
}

/// Starts a queue of `op` runs, one utility at a time. Progress arrives as the
/// `run-item`, `run-output`, `askpass-request` and `run-done` events.
#[tauri::command]
pub fn run_action(
    app: AppHandle,
    state: State<'_, RunState>,
    op: String,
    names: Vec<String>,
    dry_run: bool,
) -> Result<(), String> {
    if !OPS.contains(&op.as_str()) {
        return Err(format!("Unknown action: {op}"));
    }
    if names.is_empty() {
        return Err("Nothing selected".to_string());
    }
    let script = staged_script(&app, &crate::script_path(&app)?)?;
    spawn_run(&app, &state, move |app| {
        run_queue(app, &names, &|name| {
            let mut cmd = crate::script_command(app, &script);
            if dry_run {
                cmd.arg("--dry-run");
            }
            cmd.args(["--no-color", &format!("--{op}")]);
            cmd.arg(name);
            cmd
        });
    })
}

/// Marks a run as started and does `work` on a thread; fails when another run
/// is going. `work` must end the run in the UI with `run-done`.
pub(crate) fn spawn_run(
    app: &AppHandle,
    state: &State<'_, RunState>,
    work: impl FnOnce(&AppHandle) + Send + 'static,
) -> Result<(), String> {
    {
        let mut inner = state.inner.lock().unwrap();
        if inner.running {
            return Err("Another action is already running".to_string());
        }
        *inner = Inner { running: true, ..Inner::default() };
    }
    let app = app.clone();
    std::thread::spawn(move || {
        work(&app);
        app.state::<RunState>().inner.lock().unwrap().running = false;
    });
    Ok(())
}

/// The password dialog's answer; `None` means the user cancelled it.
#[tauri::command]
pub fn askpass_reply(state: State<'_, RunState>, password: Option<String>) {
    if let Some(tx) = state.inner.lock().unwrap().reply.take() {
        let _ = tx.send(password);
    }
}

/// True when the system has flagged a pending reboot: the same two checks
/// linux_util's menu uses before it offers one (the Debian marker file, and
/// `needs-restarting -r` on Fedora/RHEL).
#[tauri::command]
pub fn reboot_needed() -> bool {
    if Path::new("/var/run/reboot-required").exists() {
        return true;
    }
    let mut cmd = Command::new("needs-restarting");
    crate::use_host_env(&mut cmd);
    cmd.arg("-r").stdout(Stdio::null()).stderr(Stdio::null());
    matches!(cmd.status(), Ok(s) if !s.success())
}

/// Runs `systemctl reboot` through `sudo`, feeding it `password` when given and
/// otherwise running with `-n` so it never waits for one. Err carries sudo's
/// stderr, or why it could not be started.
fn reboot_with_sudo(sudo: PathBuf, password: Option<&str>) -> Result<(), String> {
    let mut cmd = Command::new(sudo);
    match password {
        Some(_) => cmd.args(["-S", "-k", "-p", "", "systemctl", "reboot"]),
        None => cmd.args(["-n", "systemctl", "reboot"]),
    };
    crate::use_host_env(&mut cmd);
    let mut child = cmd
        .stdin(if password.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run the reboot command: {e}"))?;
    if let (Some(pw), Some(mut stdin)) = (password, child.stdin.take()) {
        let _ = writeln!(stdin, "{pw}");
    }
    let out = child.wait_with_output().map_err(|e| format!("Reboot failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Reboots the machine. With `sudo` available it uses it: with the password
/// the last run was given, or, when none was needed (passwordless sudo), with
/// `-n`. Only when that fails, or there is no `sudo`, does it run `systemctl
/// reboot` as the signed-in user and let the desktop's own policy decide; that
/// is denied where there is no polkit agent, such as WSL.
#[tauri::command]
pub fn reboot_now(state: State<'_, RunState>) -> Result<(), String> {
    let password = state.inner.lock().unwrap().sudo_password.take();
    if let Some(sudo) = find_in_path("sudo") {
        if reboot_with_sudo(sudo, password.as_deref()).is_ok() {
            return Ok(());
        }
    }
    let mut cmd = Command::new("systemctl");
    cmd.arg("reboot");
    crate::use_host_env(&mut cmd);
    let out = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Could not run the reboot command: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Drops the remembered sudo password when the reboot offer is dismissed.
#[tauri::command]
pub fn forget_password(state: State<'_, RunState>) {
    state.inner.lock().unwrap().sudo_password = None;
}

/// Sends one line typed in the run panel to the running script's stdin. The
/// partial line it answers is taken as ended, so the script's next output
/// starts a new line, as it would after Enter in a terminal.
#[tauri::command]
pub fn send_input(state: State<'_, RunState>, text: String) -> Result<(), String> {
    let mut inner = state.inner.lock().unwrap();
    let stdin = inner.stdin.as_mut().ok_or("Nothing is running")?;
    writeln!(stdin, "{text}")
        .and_then(|_| stdin.flush())
        .map_err(|e| format!("Could not send the answer: {e}"))?;
    for pending in &inner.pending {
        pending.lock().unwrap().clear();
    }
    Ok(())
}

/// Stops the queue and terminates the running script (SIGTERM, then SIGKILL after 5 s).
#[tauri::command]
pub fn cancel_run(state: State<'_, RunState>) {
    let pid = {
        let mut inner = state.inner.lock().unwrap();
        if !inner.running {
            return;
        }
        inner.cancelled = true;
        if let Some(tx) = inner.reply.take() {
            let _ = tx.send(None);
        }
        inner.pid
    };
    let Some(pid) = pid else { return };
    let group = -(pid as i32);
    // Commands the script runs under `timeout` (the package cache refresh) sit
    // in a process group of their own, so the group signal alone never reaches
    // them and the script, waiting on one, cannot act on it until it ends.
    let others = descendants(pid);
    unsafe { libc::kill(group, libc::SIGTERM) };
    for &p in &others {
        unsafe { libc::kill(p as i32, libc::SIGTERM) };
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        // Signal 0 only checks that the group still exists.
        if unsafe { libc::kill(group, 0) } == 0 {
            unsafe { libc::kill(group, libc::SIGKILL) };
        }
        for p in others {
            unsafe { libc::kill(p as i32, libc::SIGKILL) };
        }
    });
}

/// Every process below `root` in the process tree, read from `/proc`.
fn descendants(root: u32) -> Vec<u32> {
    let mut parent_of = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // The command name in parentheses may hold spaces; the parent pid
            // is the second field after its closing bracket.
            let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else { continue };
            if let Some(ppid) = rest.split_whitespace().nth(1).and_then(|s| s.parse::<u32>().ok()) {
                parent_of.push((pid, ppid));
            }
        }
    }
    let mut found = vec![root];
    let mut i = 0;
    while i < found.len() {
        let parent = found[i];
        found.extend(parent_of.iter().filter(|&&(_, pp)| pp == parent).map(|&(p, _)| p));
        i += 1;
    }
    found.remove(0);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A script that asks with `read < /dev/tty` (as linux_util's installers
    /// do) gets the answer written to the pty master, with no app terminal.
    #[test]
    fn script_can_read_answer_from_dev_tty() {
        let pty = Pty::open().unwrap();
        let mut cmd = Command::new("bash");
        cmd.args(["-c", "read -rp 'ok? ' a < /dev/tty && echo \"got:$a\""])
            .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        let Pty { mut master, slave } = pty;
        drop(slave);
        writeln!(master, "y").unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "got:y");
        assert!(out.status.success());
    }

    /// A script that exits leaving a background process on its terminal and
    /// output (as a driver install's module build can) does not hold the run:
    /// the readers are given up on after the grace period.
    #[test]
    fn background_process_does_not_hold_the_run() {
        let pty = Pty::open().unwrap();
        let mut cmd = Command::new("bash");
        cmd.args(["-c", "sleep 30 & echo $!"])
            .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        let Pty { master, slave } = pty;
        drop(slave);
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let (line_tx, line_rx) = mpsc::channel::<String>();
        let mut stdout = child.stdout.take().unwrap();
        let done = done_tx.clone();
        std::thread::spawn(move || {
            let _done = done;
            let mut text = String::new();
            let mut buf = [0u8; 64];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                text.push_str(&String::from_utf8_lossy(&buf[..n]));
                if text.ends_with('\n') {
                    let _ = line_tx.send(text.trim().to_string());
                }
            }
        });
        let done = done_tx.clone();
        std::thread::spawn(move || {
            let _done = done;
            let mut master = master;
            let mut buf = [0u8; 64];
            while matches!(master.read(&mut buf), Ok(n) if n > 0) {}
        });
        drop(done_tx);
        assert!(child.wait().unwrap().success());
        let started = std::time::Instant::now();
        let all_ended = wait_for_readers(&done_rx, Duration::from_millis(500));
        let elapsed = started.elapsed();
        let sleeper: i32 = line_rx.recv_timeout(Duration::from_secs(1)).unwrap().parse().unwrap();
        unsafe { libc::kill(sleeper, libc::SIGKILL) };
        assert!(!all_ended, "the background sleep should still hold the output");
        assert!(elapsed < Duration::from_secs(2), "waited {elapsed:?}");
    }
}
