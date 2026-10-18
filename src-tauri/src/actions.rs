//! Install, uninstall and update runs.
//!
//! `linux_util.sh` refuses to run as root and calls `sudo` itself, so it runs
//! as the normal user with a `sudo` shim first on `PATH` that adds `-A`.
//! `SUDO_ASKPASS` points at this same executable started with `--askpass`; it
//! asks the running app for the password over a private Unix socket and the
//! app shows its own dialog. The password is asked once, when the app starts
//! (as linux_util asks before its menu), kept in memory only, and forgotten
//! once sudo's own timeout has passed with nothing running.
//!
//! Each script runs on its own pseudo-terminal, which is its stdin, stdout,
//! stderr and controlling terminal, so it behaves as it does in a terminal
//! window. The run panel draws that terminal's output and shows the answer box
//! only while a question is waiting.

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
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

const OPS: [&str; 3] = ["install", "uninstall", "update"];
const SOCKET_ENV: &str = "LU_ASKPASS_SOCK";

#[derive(Default)]
pub struct RunState {
    inner: Mutex<Inner>,
    session: Mutex<Session>,
}

/// The sudo password, held for the app session the way a terminal holds a sudo
/// login. Kept apart from `Inner`, which each run starts afresh.
#[derive(Default)]
struct Session {
    password: Option<String>,
    /// sudo's `timestamp_timeout`, read when the password is accepted. `None`
    /// means never forget (sudo's negative timeout).
    timeout: Option<Duration>,
    /// When the last run ended, or the password was given outside a run.
    /// `None` while a run is going, so a run never times out half way.
    idle_since: Option<Instant>,
}

impl Session {
    /// The held password, forgetting it first if the timeout has passed.
    fn password(&mut self) -> Option<String> {
        if let (Some(since), Some(limit)) = (self.idle_since, self.timeout) {
            if since.elapsed() >= limit {
                self.password = None;
            }
        }
        self.password.clone()
    }
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
    /// False when no pty could be opened and `stdin` is a plain pipe.
    stdin_is_pty: bool,
}

/// A line from the app itself rather than the script, such as an update's
/// progress or a failure to start.
#[derive(Serialize, Clone)]
struct Output<'a> {
    text: &'a str,
    /// Shown in the error colour.
    error: bool,
}

/// Raw terminal output for the run panel to draw, escape sequences included.
#[derive(Serialize, Clone)]
struct TermOutput<'a> {
    data: &'a str,
}

/// Whether a question is waiting for an answer, and how it is answered:
/// `mode` "line" (text and Enter) or "key" (keys sent as typed, no Enter).
#[derive(Serialize, Clone)]
struct InputEvent {
    waiting: bool,
    mode: &'static str,
    hidden: bool,
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
# ISO's installer) would lose the app's `read`; load it inside root's bash.
if [ "$1" = bash ]; then
    shift
    exec {REAL_SUDO} -A bash -c '. {PRELUDE}; export -f read; exec bash "$@"' bash "$@"
fi
exec {REAL_SUDO} -A "$@"
"#;

/// Sourced by every bash the script starts (`BASH_ENV`). It changes nothing
/// about what `read` does; it only lets the app follow it.
///
/// On the app's terminal it tells the run panel that a question is waiting and
/// how it is answered, with a private escape sequence the app strips from the
/// output: `ESC ] 7701 ; wait ; line|key ; 0|1 BEL` before the read (key when
/// `-n`/`-N` read a fixed number of keys, 1 when `-s` hides the input) and
/// `ESC ] 7701 ; done BEL` after it. Reads that are not from the terminal (a
/// file, a FIFO) and short timed reads (linux_util drains Esc sequences with
/// `-t 0.05`) are left alone.
///
/// Without a pty, stdin is a pipe and bash prints no `read -p` prompt, so the
/// prompt is printed to stderr instead. Handles `-p` as its own option or last
/// in a cluster (`-rp`), which is how linux_util writes them.
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
    local __lu_fd=0 __lu_mode=line __lu_hidden=0 __lu_t="" __lu_want="" __lu_a __lu_i __lu_c
    for __lu_a in "$@"; do
        if [ -n "$__lu_want" ]; then
            case "$__lu_want" in
                u) __lu_fd=$__lu_a ;;
                t) __lu_t=$__lu_a ;;
            esac
            __lu_want=""
            continue
        fi
        case "$__lu_a" in
            --) break ;;
            -?*) ;;
            *) break ;;
        esac
        __lu_i=1
        while [ "$__lu_i" -lt "${#__lu_a}" ]; do
            __lu_c=${__lu_a:__lu_i:1}
            __lu_i=$((__lu_i + 1))
            case "$__lu_c" in
                s) __lu_hidden=1 ;;
                n|N) __lu_mode=key ;;
            esac
            case "$__lu_c" in
                a|d|i|n|N|p|t|u)
                    if [ "$__lu_i" -lt "${#__lu_a}" ]; then
                        case "$__lu_c" in
                            u) __lu_fd=${__lu_a:__lu_i} ;;
                            t) __lu_t=${__lu_a:__lu_i} ;;
                        esac
                    else
                        __lu_want=$__lu_c
                    fi
                    break
                    ;;
            esac
        done
    done
    case "$__lu_t" in
        0|0.*|.*) builtin read "$@"; return ;;
    esac
    if ! [ -t "$__lu_fd" ] || ! { : >/dev/tty; } 2>/dev/null; then
        builtin read "$@"
        return
    fi
    printf '\033]7701;wait;%s;%s\007' "$__lu_mode" "$__lu_hidden" >/dev/tty
    builtin read "$@"
    local __lu_rc=$?
    printf '\033]7701;done\007' >/dev/tty 2>/dev/null
    return $__lu_rc
}
"#;

/// A pseudo-terminal for one script. linux_util is written for a terminal: its
/// installers ask with `read ... < /dev/tty`, and it switches to colours and
/// spinners when its output is one. The script gets the slave as stdin,
/// stdout, stderr and controlling terminal, so everything it writes arrives on
/// the master in order, as a terminal window receives it. Answers typed in the
/// run panel are written to the master, and the terminal echoes them as usual
/// (or not, when the program turns echo off for a password).
struct Pty {
    master: File,
    slave: File,
}

/// The terminal size the script is told. Programs that draw to the width (dnf
/// and apt progress bars, tables) lay out for this many columns.
const PTY_COLS: u16 = 120;
const PTY_ROWS: u16 = 40;

impl Pty {
    fn open() -> std::io::Result<Self> {
        let (mut m, mut sl) = (0, 0);
        let size = libc::winsize { ws_row: PTY_ROWS, ws_col: PTY_COLS, ws_xpixel: 0, ws_ypixel: 0 };
        let rc = unsafe { libc::openpty(&mut m, &mut sl, std::ptr::null_mut(), std::ptr::null(), &size) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let (master, slave) = unsafe { (File::from_raw_fd(m), File::from_raw_fd(sl)) };
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

/// True when sudo is set up to run without a password (NOPASSWD). A login
/// cached from earlier does not count (`-k`): it is tied to another terminal,
/// so the scripts' own terminals could not use it.
fn sudo_without_password(real_sudo: &Path) -> bool {
    let mut cmd = Command::new(real_sudo);
    crate::use_host_env(&mut cmd);
    cmd.args(["-n", "-k", "true"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    matches!(cmd.status(), Ok(s) if s.success())
}

/// sudo's default `timestamp_timeout`, used when the configured one can't be read.
const SUDO_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// sudo's `timestamp_timeout`, as `sudo -V` reports it when run as root
/// ("Authentication timestamp timeout: 5.0 minutes"). `None` means never time
/// out (a negative setting). Falls back to sudo's default of 5 minutes.
fn sudo_timeout(real_sudo: &Path, password: &str) -> Option<Duration> {
    let mut cmd = Command::new(real_sudo);
    crate::use_host_env(&mut cmd);
    let child = cmd
        .env("LC_ALL", "C")
        .args(["-S", "-p", ""])
        .arg(real_sudo)
        .arg("-V")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return Some(SUDO_DEFAULT_TIMEOUT) };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{password}");
    }
    let text = child.wait_with_output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    parse_sudo_timeout(&text)
}

fn parse_sudo_timeout(sudo_v: &str) -> Option<Duration> {
    let minutes = sudo_v
        .lines()
        .find_map(|l| l.trim().strip_prefix("Authentication timestamp timeout:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok());
    match minutes {
        Some(m) if m < 0.0 => None,
        Some(m) if m.is_finite() => Some(Duration::from_secs_f64(m * 60.0)),
        _ => Some(SUDO_DEFAULT_TIMEOUT),
    }
}

/// Shows the app's password dialog until sudo accepts the password or the
/// user cancels (`None`).
fn ask_password(app: &AppHandle, real_sudo: &Path) -> Option<String> {
    let state = app.state::<RunState>();
    let mut error = None;
    let answer = loop {
        let (tx, rx) = mpsc::channel();
        state.inner.lock().unwrap().reply = Some(tx);
        let _ = app.emit("askpass-request", AskpassRequest { error: error.take() });
        match rx.recv() {
            Ok(Some(pw)) if password_ok(real_sudo, &pw) => break Some(pw),
            Ok(Some(_)) => error = Some("Wrong password, try again.".to_string()),
            _ => break None,
        }
    };
    state.inner.lock().unwrap().reply = None;
    let _ = app.emit("askpass-close", ());
    answer
}

/// Holds an accepted password for the session. Outside a run, its timeout
/// starts now.
fn keep_password(state: &RunState, real_sudo: &Path, password: String) {
    let timeout = sudo_timeout(real_sudo, &password);
    let running = state.inner.lock().unwrap().running;
    let mut session = state.session.lock().unwrap();
    session.password = Some(password);
    session.timeout = timeout;
    session.idle_since = if running { None } else { Some(Instant::now()) };
}

/// Answers each askpass request from the session's password, asking for it
/// (once) when none is held. After a cancel, the rest of the run's requests
/// fail without asking again.
fn serve_askpass(app: AppHandle, listener: UnixListener, real_sudo: PathBuf, stop: Arc<AtomicBool>) {
    let state = app.state::<RunState>();
    let mut denied = false;
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
        if denied {
            continue; // dropping the stream makes askpass exit 1
        }
        let held = state.session.lock().unwrap().password();
        let password = held.or_else(|| {
            let pw = ask_password(&app, &real_sudo)?;
            keep_password(&state, &real_sudo, pw.clone());
            Some(pw)
        });
        match password {
            Some(pw) => {
                let _ = stream.write_all(pw.as_bytes());
            }
            None => denied = true,
        }
    }
}

/// Asks for the sudo password when the app starts, as linux_util does before
/// its menu, unless sudo needs none. Cancelling leaves it to the first run.
#[tauri::command]
pub async fn start_session(app: AppHandle) {
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let Some(sudo) = find_in_path("sudo") else { return };
        if sudo_without_password(&sudo) {
            return;
        }
        if let Some(pw) = ask_password(&app, &sudo) {
            keep_password(&app.state::<RunState>(), &sudo, pw);
        }
    })
    .await;
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

/// What `--install`/`--update` print on that exit 1 straight after
/// `NO_CHANGES_LINE`. The app counts the item as succeeded, so it is not shown.
const CLI_FAILED_PREFIX: &str = "Failed: ";

/// How long output is still read after the script itself has exited. A process
/// it left running in the background keeps the pipes and terminal open for as
/// long as it lives, and the item must not stay "running" until then: in a
/// terminal, linux_util's menu carries on in the same case.
const OUTPUT_GRACE: Duration = Duration::from_secs(2);

/// Start of the private escape sequence the `read` wrapper writes (see
/// `READ_PRELUDE`); it ends at BEL.
const SIGNAL_START: &[u8] = b"\x1b]7701;";

/// Splits terminal output into what is shown and the `read` wrapper's signals,
/// across reads that may cut a signal in two.
#[derive(Default)]
struct Signals {
    held: Vec<u8>,
}

impl Signals {
    fn feed(&mut self, data: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut buf = std::mem::take(&mut self.held);
        buf.extend_from_slice(data);
        let (mut out, mut signals) = (Vec::with_capacity(buf.len()), Vec::new());
        let mut i = 0;
        while i < buf.len() {
            let rest = &buf[i..];
            if rest[0] == 0x1b {
                if rest.len() < SIGNAL_START.len() && SIGNAL_START.starts_with(rest) {
                    self.held = rest.to_vec();
                    break;
                }
                if rest.starts_with(SIGNAL_START) {
                    match rest.iter().position(|&b| b == 0x07) {
                        Some(end) => {
                            signals.push(String::from_utf8_lossy(&rest[SIGNAL_START.len()..end]).into_owned());
                            i += end + 1;
                            continue;
                        }
                        None if rest.len() < 64 => {
                            self.held = rest.to_vec();
                            break;
                        }
                        None => {} // too long to be ours: pass it through
                    }
                }
            }
            out.push(rest[0]);
            i += 1;
        }
        (out, signals)
    }
}

/// Decodes `data` as UTF-8, holding back a character cut off at the end of a
/// read until the next one completes it. Invalid bytes become U+FFFD.
fn decode_utf8(held: &mut Vec<u8>, data: &[u8]) -> String {
    held.extend_from_slice(data);
    let mut text = String::new();
    let mut rest: &[u8] = held;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                text.push_str(s);
                rest = &[];
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                text.push_str(std::str::from_utf8(good).unwrap_or_default());
                match e.error_len() {
                    Some(n) => {
                        text.push('\u{fffd}');
                        rest = &bad[n..];
                    }
                    None => {
                        rest = bad;
                        break;
                    }
                }
            }
        }
    }
    *held = rest.to_vec();
    text
}

/// How long output must have stopped, on an unfinished line that looks like a
/// question, before the run panel offers the answer box for a program other
/// than bash's `read` (`chsh` asking for a password, a package manager's own
/// `[y/N]`).
const PROMPT_IDLE: Duration = Duration::from_millis(500);

/// What the run panel knows about a question waiting on the terminal.
#[derive(Default)]
struct Prompt {
    last_output: Option<Instant>,
    /// The text of the unfinished last line; empty after a newline.
    open_line: String,
    /// A bash `read` signalled it is waiting.
    reading: bool,
    /// The answer box is shown on a guess (no signal); new output takes it away.
    guessed: bool,
}

impl Prompt {
    fn show(app: &AppHandle, mode: &'static str, hidden: bool) {
        let _ = app.emit("run-input", InputEvent { waiting: true, mode, hidden });
    }

    fn hide(app: &AppHandle) {
        let _ = app.emit("run-input", InputEvent { waiting: false, mode: "line", hidden: false });
    }

    fn output(&mut self, app: &AppHandle, line_tail: Option<&str>) {
        self.last_output = Some(Instant::now());
        match line_tail {
            Some(tail) => self.open_line = tail.to_string(),
            None => self.open_line.clear(),
        }
        if self.guessed {
            self.guessed = false;
            Self::hide(app);
        }
    }

    fn signal(&mut self, app: &AppHandle, signal: &str) {
        let mut parts = signal.split(';');
        match parts.next() {
            Some("wait") => {
                let mode = if parts.next() == Some("key") { "key" } else { "line" };
                let hidden = parts.next() == Some("1");
                self.reading = true;
                self.guessed = false;
                Self::show(app, mode, hidden);
            }
            Some("done") => {
                self.reading = false;
                Self::hide(app);
            }
            _ => {}
        }
    }

    /// Offers the answer box when output has stopped on what looks like a
    /// question.
    fn check_idle(&mut self, app: &AppHandle, master: Option<&File>) {
        if let Some((mode, hidden)) = self.idle_guess(master) {
            Self::show(app, mode, hidden);
        }
    }

    /// The answer box to offer now, if output has stopped on what looks like a
    /// question with no signal. The terminal's own mode says how it is
    /// answered: echo off is a password, line editing off is single keys.
    fn idle_guess(&mut self, master: Option<&File>) -> Option<(&'static str, bool)> {
        if self.reading || self.guessed || !looks_like_question(&self.open_line) {
            return None;
        }
        if !self.last_output.is_some_and(|t| t.elapsed() >= PROMPT_IDLE) {
            return None;
        }
        self.guessed = true;
        Some(master.map(terminal_mode).unwrap_or(("line", false)))
    }
}

/// True for the end of a line that asks something: "Password: ",
/// "Is this ok [y/N]: ", "Do you want to continue? [Y/n] ".
fn looks_like_question(line: &str) -> bool {
    matches!(line.trim_end().chars().last(), Some(':' | '?' | ']' | ')' | '>'))
}

/// How the program reading the terminal wants its answer. While sudo relays a
/// command it puts this terminal in raw mode (echo and line editing both off);
/// the command behind it still reads lines, so that counts as "line".
fn terminal_mode(master: &File) -> (&'static str, bool) {
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(master.as_raw_fd(), &mut t) } != 0 {
        return ("line", false);
    }
    let canonical = t.c_lflag & libc::ICANON != 0;
    let echo = t.c_lflag & libc::ECHO != 0;
    match (canonical, echo) {
        (true, false) => ("line", true),
        (false, true) => ("key", false),
        _ => ("line", false),
    }
}

/// Longest line kept for the "No package changes" check; the rest is dropped.
const MAX_LINE: usize = 64 * 1024;

/// Follows output line by line for the "No package changes" line, and holds
/// back the line after it until it is complete so a `CLI_FAILED_PREFIX` line
/// can be dropped.
#[derive(Default)]
struct NoChanges {
    line: Vec<u8>,
    seen: bool,
    holding: bool,
    held: String,
}

impl NoChanges {
    /// Returns the part of `text` to show now.
    fn feed(&mut self, text: &str) -> String {
        let mut shown = String::with_capacity(text.len());
        let mut start = 0;
        for (i, &b) in text.as_bytes().iter().enumerate() {
            if b != b'\n' {
                if self.line.len() < MAX_LINE {
                    self.line.push(b);
                }
                continue;
            }
            let cleaned = clean_line(&self.line);
            self.line.clear();
            if self.holding {
                self.held.push_str(&text[start..=i]);
                start = i + 1;
                self.holding = false;
                let held = std::mem::take(&mut self.held);
                if !cleaned.starts_with(CLI_FAILED_PREFIX) {
                    shown.push_str(&held);
                }
            } else if cleaned.contains(NO_CHANGES_LINE) {
                self.seen = true;
                shown.push_str(&text[start..=i]);
                start = i + 1;
                self.holding = true;
            }
        }
        if self.holding {
            self.held.push_str(&text[start..]);
        } else {
            shown.push_str(&text[start..]);
        }
        shown
    }

    /// The line being held back, once the output has ended.
    fn finish(&mut self) -> String {
        self.holding = false;
        std::mem::take(&mut self.held)
    }

    /// The line written so far, with nothing after it yet.
    fn tail(&self) -> Option<String> {
        (!self.line.is_empty()).then(|| clean_line(&self.line))
    }
}

/// Sends `reader`'s output to the run panel as it arrives, as terminal data.
/// Takes the `read` wrapper's signals out of it, notes the "No package changes"
/// line and drops the "Failed:" line after it, and keeps `prompt` up to date.
fn pump<R: Read + Send + 'static>(
    app: AppHandle,
    mut reader: R,
    prompt: Arc<Mutex<Prompt>>,
    no_changes: Arc<AtomicBool>,
    done: mpsc::Sender<()>,
    detached: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let _done = done;
        let mut signals = Signals::default();
        let mut utf8 = Vec::new();
        let mut lines = NoChanges::default();
        let mut chunk = [0u8; 4096];
        loop {
            let n = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            // Past the grace period the item has ended; keep reading so the
            // leftover process never blocks on a full buffer, but show nothing.
            if detached.load(Ordering::SeqCst) {
                continue;
            }
            let (out, sigs) = signals.feed(&chunk[..n]);
            let text = decode_utf8(&mut utf8, &out);
            if !text.is_empty() {
                let shown = lines.feed(&text);
                if lines.seen {
                    no_changes.store(true, Ordering::SeqCst);
                }
                if !shown.is_empty() {
                    let _ = app.emit("run-term", TermOutput { data: &shown });
                }
                prompt.lock().unwrap().output(&app, lines.tail().as_deref());
            }
            for signal in sigs {
                prompt.lock().unwrap().signal(&app, &signal);
            }
        }
        let rest = lines.finish();
        if !rest.is_empty() && !detached.load(Ordering::SeqCst) {
            let _ = app.emit("run-term", TermOutput { data: &rest });
        }
    });
}

/// Checks for a question waiting without a signal until `finished`, then
/// takes the answer box away.
fn watch_prompt(app: AppHandle, prompt: Arc<Mutex<Prompt>>, master: Option<File>, finished: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !finished.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(150));
            prompt.lock().unwrap().check_idle(&app, master.as_ref());
        }
        Prompt::hide(&app);
    });
}

/// Waits until every output reader has ended or `grace` has passed, and says
/// which. Each reader holds a sender, so the channel disconnects once all end.
fn wait_for_readers(done: &mpsc::Receiver<()>, grace: Duration) -> bool {
    matches!(done.recv_timeout(grace), Err(mpsc::RecvTimeoutError::Disconnected))
}

/// Tells the UI a line of the app's own output, in the error colour when `error`.
pub(crate) fn emit_output(app: &AppHandle, text: &str, error: bool) {
    let _ = app.emit("run-output", Output { text, error });
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
    {
        // The run starts now: let a timed-out password go, then hold the rest
        // until it ends.
        let mut session = state.session.lock().unwrap();
        session.password();
        session.idle_since = None;
    }
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
            // Nobody can page through output here, so pagers print and carry on.
            .env("PAGER", "cat")
            .env("SYSTEMD_PAGER", "cat")
            .env("GIT_PAGER", "cat");
        let slaves = pty.as_ref().and_then(|p| {
            Some([p.slave.try_clone().ok()?, p.slave.try_clone().ok()?, p.slave.try_clone().ok()?])
        });
        let on_pty = slaves.is_some();
        match slaves {
            Some([stdin, stdout, stderr]) => {
                cmd.env("TERM", "xterm-256color")
                    .stdin(Stdio::from(stdin))
                    .stdout(Stdio::from(stdout))
                    .stderr(Stdio::from(stderr));
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
                cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
            }
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("Could not start {}: {e}", cmd.get_program().to_string_lossy());
                let _ = app.emit("run-output", Output { text: &msg, error: true });
                let _ = app.emit("run-item", ItemEvent { index, name, state: "failed", code: None });
                failed += 1;
                continue;
            }
        };
        let pty = pty.filter(|_| on_pty);
        {
            let mut inner = state.inner.lock().unwrap();
            inner.pid = Some(child.id());
            inner.stdin_is_pty = pty.is_some();
            inner.stdin = match &pty {
                Some(p) => p.master.try_clone().ok(),
                None => None,
            }
            .or_else(|| {
                // No pty: the pipe's write end, wrapped as a File.
                child.stdin.take().map(|s| unsafe { File::from_raw_fd(s.into_raw_fd()) })
            });
        }
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let detached = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let prompt = Arc::new(Mutex::new(Prompt::default()));
        let no_changes = Arc::new(AtomicBool::new(false));
        // Reading the master ends with EIO once the script and its children exit.
        let mut master_for_mode = None;
        match pty {
            Some(Pty { master, slave }) => {
                drop(slave);
                master_for_mode = master.try_clone().ok();
                pump(app.clone(), master, prompt.clone(), no_changes.clone(), done_tx.clone(), detached.clone());
            }
            None => {
                if let Some(out) = child.stdout.take() {
                    pump(app.clone(), out, prompt.clone(), no_changes.clone(), done_tx.clone(), detached.clone());
                }
                if let Some(err) = child.stderr.take() {
                    pump(app.clone(), err, prompt.clone(), no_changes.clone(), done_tx.clone(), detached.clone());
                }
            }
        }
        drop(done_tx);
        watch_prompt(app.clone(), prompt, master_for_mode, finished.clone());
        let status = child.wait();
        state.inner.lock().unwrap().stdin = None; // closing it lets a child left reading stdin see EOF
        // Readers still going after this belong to a background process the
        // script left behind; they carry on, detached, until it closes them.
        wait_for_readers(&done_rx, OUTPUT_GRACE);
        detached.store(true, Ordering::SeqCst);
        finished.store(true, Ordering::SeqCst);
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
    if let Some(server) = server {
        let _ = server.join();
    }
    state.session.lock().unwrap().idle_since = Some(Instant::now());
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
            cmd.arg(format!("--{op}"));
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

/// Reboots the machine. With `sudo` available it uses it: with the session's
/// password while it is held, or otherwise (passwordless sudo) with `-n`. Only when that fails, or there is no `sudo`, does it run `systemctl
/// reboot` as the signed-in user and let the desktop's own policy decide; that
/// is denied where there is no polkit agent, such as WSL.
#[tauri::command]
pub fn reboot_now(state: State<'_, RunState>) -> Result<(), String> {
    let password = state.session.lock().unwrap().password();
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

/// Sends what was typed in the run panel to the running script's terminal, as
/// a terminal sends keys: `text` as it is, then Enter when `enter` is set. A
/// single-key question gets its key with no Enter, so nothing is left over
/// for the next question.
#[tauri::command]
pub fn send_input(state: State<'_, RunState>, text: String, enter: bool) -> Result<(), String> {
    let mut inner = state.inner.lock().unwrap();
    // A terminal's Enter is CR, which the pty turns into a newline for the
    // reader; a plain pipe gets the newline itself.
    let end = match (enter, inner.stdin_is_pty) {
        (false, _) => "",
        (true, true) => "\r",
        (true, false) => "\n",
    };
    let stdin = inner.stdin.as_mut().ok_or("Nothing is running")?;
    stdin
        .write_all(format!("{text}{end}").as_bytes())
        .and_then(|_| stdin.flush())
        .map_err(|e| format!("Could not send the answer: {e}"))
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

    /// A bash script on a fresh pty (stdin, stdout, stderr and controlling
    /// terminal), with the app's `read` wrapper loaded, as a run starts it.
    /// Returns the child, the master, and everything read from the master.
    fn on_pty(script: &str) -> (std::process::Child, File, Arc<Mutex<Vec<u8>>>) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let prelude = std::env::temp_dir().join(format!(
            "lud-prelude-{}-{}.sh",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&prelude, READ_PRELUDE).unwrap();
        let pty = Pty::open().unwrap();
        let mut cmd = Command::new("bash");
        cmd.args(["-c", script])
            .env("BASH_ENV", &prelude)
            .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::from(pty.slave.try_clone().unwrap()));
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        let Pty { master, slave } = pty;
        drop(slave);
        let out = Arc::new(Mutex::new(Vec::new()));
        let (mut reader, sink) = (master.try_clone().unwrap(), out.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        (child, master, out)
    }

    fn count(out: &Arc<Mutex<Vec<u8>>>, needle: &str) -> usize {
        String::from_utf8_lossy(&out.lock().unwrap()).matches(needle).count()
    }

    /// Waits up to 5 s for `needle` to have appeared `times` times.
    fn wait_for(out: &Arc<Mutex<Vec<u8>>>, needle: &str, times: usize) {
        let start = Instant::now();
        while count(out, needle) < times {
            assert!(start.elapsed() < Duration::from_secs(5), "never saw {needle:?} x{times} in {:?}", text(out));
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn text(out: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8_lossy(&out.lock().unwrap()).into_owned()
    }

    fn finish(mut child: std::process::Child, out: &Arc<Mutex<Vec<u8>>>, needle: &str) {
        wait_for(out, needle, 1);
        assert!(child.wait().unwrap().success());
    }

    /// Two single-key questions in a row (Snapper, Timeshift, Bootloader
    /// Manager): each gets the key typed for it. Sending the key with Enter, as
    /// the app used to, answered the second question with the leftover Enter.
    #[test]
    fn single_key_questions_each_get_their_own_key() {
        let (child, mut master, out) = on_pty(
            "read -n 1 -rp 'First? (y/N) ' a < /dev/tty; echo; \
             read -n 1 -rp 'Second? (y/N) ' b < /dev/tty; echo; echo \"got:$a$b\"",
        );
        wait_for(&out, "\x1b]7701;wait;key;0\x07", 1);
        master.write_all(b"y").unwrap();
        wait_for(&out, "\x1b]7701;wait;key;0\x07", 2);
        master.write_all(b"n").unwrap();
        finish(child, &out, "got:yn");
        assert_eq!(count(&out, "\x1b]7701;done\x07"), 2);
    }

    /// A typed answer to a line question, such as a menu number, reaches the
    /// script and is echoed after the prompt by the terminal itself.
    #[test]
    fn line_question_gets_text_and_enter_and_is_echoed() {
        let (child, mut master, out) = on_pty("read -rp 'Select (1-4): ' c < /dev/tty; echo \"got:$c\"");
        wait_for(&out, "\x1b]7701;wait;line;0\x07", 1);
        wait_for(&out, "Select (1-4): ", 1);
        master.write_all(b"2\r").unwrap();
        finish(child, &out, "got:2");
        assert!(text(&out).contains("Select (1-4): 2"), "{:?}", text(&out));
    }

    /// A hidden question (Mount SMB Share's password) is signalled as hidden
    /// and what is typed never comes back as output.
    #[test]
    fn hidden_question_is_signalled_and_not_echoed() {
        let (child, mut master, out) =
            on_pty("read -rsp 'Password (hidden): ' p < /dev/tty; echo; echo \"len:${#p}\"");
        wait_for(&out, "\x1b]7701;wait;line;1\x07", 1);
        master.write_all(b"s3cret\r").unwrap();
        finish(child, &out, "len:6");
        assert!(!text(&out).contains("s3cret"));
    }

    /// Reads that are not questions send no signal: a short timed read (the
    /// Esc drain), a read from a file, a read from a pipe.
    #[test]
    fn reads_that_are_not_questions_send_no_signal() {
        let file = std::env::temp_dir().join(format!("lud-read-{}", std::process::id()));
        std::fs::write(&file, "from-file\n").unwrap();
        let (child, _master, out) = on_pty(&format!(
            "read -r -n 10 -t 0.05 x < /dev/tty; read -r y < {}; echo piped | {{ read -r z; echo \"z:$z\"; }}; echo \"y:$y\"",
            shell_quote(&file.to_string_lossy())
        ));
        finish(child, &out, "y:from-file");
        assert!(text(&out).contains("z:piped"));
        assert_eq!(count(&out, "7701;"), 0, "{:?}", text(&out));
        let _ = std::fs::remove_file(file);
    }

    /// A program other than bash's `read` that turns echo off and asks (as
    /// `chsh` does for the password) is guessed as a hidden question once its
    /// output has stopped, and the typed password is not echoed.
    #[test]
    fn password_prompt_from_another_program_is_guessed_hidden() {
        let (child, mut master, out) =
            on_pty("stty -echo; printf 'Password: '; builtin read -r p; stty echo; echo; echo \"len:${#p}\"");
        wait_for(&out, "Password: ", 1);
        std::thread::sleep(Duration::from_millis(100));
        let mut prompt = Prompt {
            last_output: Some(Instant::now() - PROMPT_IDLE),
            open_line: "Password: ".to_string(),
            ..Prompt::default()
        };
        assert_eq!(prompt.idle_guess(Some(&master)), Some(("line", true)));
        assert_eq!(prompt.idle_guess(Some(&master)), None, "offered once");
        master.write_all(b"abc\r").unwrap();
        finish(child, &out, "len:3");
        assert!(!text(&out).contains("abc"));
    }

    #[test]
    fn idle_guess_waits_for_quiet_and_a_question() {
        let mut prompt = Prompt { last_output: Some(Instant::now()), open_line: "Continue? [y/N] ".into(), ..Prompt::default() };
        assert_eq!(prompt.idle_guess(None), None, "output only just stopped");
        prompt.last_output = Some(Instant::now() - PROMPT_IDLE);
        prompt.open_line = "Downloading packages...".into();
        assert_eq!(prompt.idle_guess(None), None, "not a question");
        prompt.open_line = "Continue? [y/N] ".into();
        prompt.reading = true;
        assert_eq!(prompt.idle_guess(None), None, "bash read already signalled");
        prompt.reading = false;
        assert_eq!(prompt.idle_guess(None), Some(("line", false)));
    }

    #[test]
    fn questions_are_recognised_by_their_ending() {
        for q in ["Password: ", "Is this ok [y/N]: ", "Do you want to continue? [Y/n] ", "Select (1-4)", "> "] {
            assert!(looks_like_question(q), "{q:?}");
        }
        for q in ["", "Downloading...", "  ⠋  Installing packages", "[INFO] Done"] {
            assert!(!looks_like_question(q), "{q:?}");
        }
    }

    #[test]
    fn signals_are_found_across_reads_and_removed() {
        let mut s = Signals::default();
        let (out, sigs) = s.feed(b"Proceed? \x1b]77");
        assert_eq!((out.as_slice(), sigs.len()), (&b"Proceed? "[..], 0));
        let (out, sigs) = s.feed(b"01;wait;key;0\x07rest\x1b[31mred");
        assert_eq!(out, b"rest\x1b[31mred");
        assert_eq!(sigs, vec!["wait;key;0".to_string()]);
        let (out, sigs) = s.feed(b"\x1b]0;title\x07\x1b]7701;done\x07");
        assert_eq!(out, b"\x1b]0;title\x07");
        assert_eq!(sigs, vec!["done".to_string()]);
    }

    #[test]
    fn failed_line_after_no_changes_is_dropped() {
        let mut n = NoChanges::default();
        assert_eq!(n.feed("[INFO] System updates completed.\r\n[INFO] No package"), "[INFO] System updates completed.\r\n[INFO] No package");
        assert!(!n.seen);
        assert_eq!(n.feed(" changes were made.\r\nFail"), " changes were made.\r\n");
        assert!(n.seen);
        assert_eq!(n.feed("ed: System Updates\r\n"), "");
        assert_eq!(n.finish(), "");

        let mut n = NoChanges::default();
        assert_eq!(n.feed("\x1b[1;34m[INFO]\x1b[0m No package changes were made.\r\nFailed: System Updates\r\n"), "\x1b[1;34m[INFO]\x1b[0m No package changes were made.\r\n");

        // Only the line straight after it, and only a "Failed: " line.
        let mut n = NoChanges::default();
        assert_eq!(n.feed("No package changes were made.\nother\nFailed: X\n"), "No package changes were made.\nother\nFailed: X\n");
        let mut n = NoChanges::default();
        assert_eq!(n.feed("Failed: X\n"), "Failed: X\n");
        let mut n = NoChanges::default();
        assert_eq!(n.feed("No package changes were made.\nleft"), "No package changes were made.\n");
        assert_eq!(n.finish(), "left");
    }

    #[test]
    fn utf8_cut_between_reads_is_kept_whole() {
        let mut held = Vec::new();
        let check = "✓ done".as_bytes();
        assert_eq!(decode_utf8(&mut held, &check[..2]), "");
        assert_eq!(decode_utf8(&mut held, &check[2..]), "✓ done");
        assert_eq!(decode_utf8(&mut held, b"a\xffb"), "a\u{fffd}b");
        assert!(held.is_empty());
    }

    #[test]
    fn sudo_timeout_is_read_from_sudo_v() {
        let v = "Sudo version 1.9.17\nSudoers policy plugin version 1.9.17\n  Authentication timestamp timeout: 15.0 minutes\n";
        assert_eq!(parse_sudo_timeout(v), Some(Duration::from_secs(15 * 60)));
        assert_eq!(parse_sudo_timeout("Authentication timestamp timeout: 0.0 minutes"), Some(Duration::ZERO));
        assert_eq!(parse_sudo_timeout("Authentication timestamp timeout: -1.0 minutes"), None);
        assert_eq!(parse_sudo_timeout("Sudo version 1.9.17"), Some(SUDO_DEFAULT_TIMEOUT));
    }

    #[test]
    fn session_password_times_out_only_when_idle() {
        let mut s = Session {
            password: Some("pw".into()),
            timeout: Some(Duration::from_secs(60)),
            idle_since: None,
        };
        assert_eq!(s.password().as_deref(), Some("pw"), "a run is going");
        s.idle_since = Some(Instant::now() - Duration::from_secs(30));
        assert_eq!(s.password().as_deref(), Some("pw"), "within the timeout");
        s.idle_since = Some(Instant::now() - Duration::from_secs(61));
        assert_eq!(s.password(), None, "timed out");
        s.password = Some("pw".into());
        s.timeout = None;
        assert_eq!(s.password().as_deref(), Some("pw"), "never times out");
    }
}
