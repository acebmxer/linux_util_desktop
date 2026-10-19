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

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

mod output;
mod sudo;
mod terminal;

use output::*;
use sudo::*;
use terminal::*;

pub(crate) use output::clean_line;
pub use sudo::askpass_client;

const OPS: [&str; 3] = ["install", "uninstall", "update"];
const SOCKET_ENV: &str = "LU_ASKPASS_SOCK";

/// Locks `m`. If a thread panicked while holding it, the data is still used:
/// every field here stays valid between statements, and refusing the lock
/// would make every later command panic too.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

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

pub(crate) fn find_in_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&crate::host_var("PATH")?).map(|d| d.join(program)).find(|p| p.is_file())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    Ok(app.path().app_data_dir().map_err(|e| format!("No app data directory: {e}"))?.join("linux_util"))
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
    let version = format!("{} {size}", std::fs::read_to_string(src.join("linux_util.VERSION")).unwrap_or_default().trim());
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
    lock(&app.state::<RunState>().inner).cancelled
}

/// Runs the commands `make` builds, one per name, with a `sudo` that asks the
/// password through the app's own dialog. Emits the run events, including the
/// final `run-done`. `make` returns the command before the environment for
/// the shim is added.
pub(crate) fn run_queue(app: &AppHandle, names: &[String], make: &dyn Fn(&str) -> Command) {
    let state = app.state::<RunState>();
    let (mut ok, mut failed) = (0, 0);

    let real_sudo = find_in_path("sudo");
    let setup =
        real_sudo.ok_or_else(|| "sudo was not found on PATH".to_string()).and_then(|sudo| Workdir::create(&sudo).map(|w| (sudo, w)));
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
        let mut session = lock(&state.session);
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
        if lock(&state.inner).cancelled {
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
        let slaves = pty.as_ref().and_then(|p| Some([p.slave.try_clone().ok()?, p.slave.try_clone().ok()?, p.slave.try_clone().ok()?]));
        let on_pty = slaves.is_some();
        match slaves {
            Some([stdin, stdout, stderr]) => {
                cmd.env("TERM", "xterm-256color").stdin(Stdio::from(stdin)).stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr));
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
            let mut inner = lock(&state.inner);
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
        // Closing it lets a child left reading stdin see EOF.
        lock(&state.inner).stdin = None;
        // Readers still going after this belong to a background process the
        // script left behind; they carry on, detached, until it closes them.
        wait_for_readers(&done_rx, OUTPUT_GRACE);
        detached.store(true, Ordering::SeqCst);
        finished.store(true, Ordering::SeqCst);
        let cancelled = {
            let mut inner = lock(&state.inner);
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
    lock(&state.session).idle_since = Some(Instant::now());
    let cancelled = lock(&state.inner).cancelled;
    let _ = app.emit("run-done", Done { ok, failed, cancelled });
}

/// Starts a queue of `op` runs, one utility at a time. Progress arrives as the
/// `run-item`, `run-output`, `askpass-request` and `run-done` events.
#[tauri::command]
pub fn run_action(app: AppHandle, state: State<'_, RunState>, op: String, names: Vec<String>, dry_run: bool) -> Result<(), String> {
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
        let mut inner = lock(&state.inner);
        if inner.running {
            return Err("Another action is already running".to_string());
        }
        *inner = Inner { running: true, ..Inner::default() };
    }
    let app = app.clone();
    std::thread::spawn(move || {
        work(&app);
        lock(&app.state::<RunState>().inner).running = false;
    });
    Ok(())
}

/// The password dialog's answer; `None` means the user cancelled it.
#[tauri::command]
pub fn askpass_reply(state: State<'_, RunState>, password: Option<String>) {
    if let Some(tx) = lock(&state.inner).reply.take() {
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
    let password = lock(&state.session).password();
    if let Some(sudo) = find_in_path("sudo") {
        if reboot_with_sudo(sudo, password.as_deref()).is_ok() {
            return Ok(());
        }
    }
    let mut cmd = Command::new("systemctl");
    cmd.arg("reboot");
    crate::use_host_env(&mut cmd);
    let out = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).output().map_err(|e| format!("Could not run the reboot command: {e}"))?;
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
    let mut inner = lock(&state.inner);
    // A terminal's Enter is CR, which the pty turns into a newline for the
    // reader; a plain pipe gets the newline itself.
    let end = match (enter, inner.stdin_is_pty) {
        (false, _) => "",
        (true, true) => "\r",
        (true, false) => "\n",
    };
    let stdin = inner.stdin.as_mut().ok_or("Nothing is running")?;
    stdin.write_all(format!("{text}{end}").as_bytes()).and_then(|_| stdin.flush()).map_err(|e| format!("Could not send the answer: {e}"))
}

/// Stops the queue and terminates the running script (SIGTERM, then SIGKILL after 5 s).
#[tauri::command]
pub fn cancel_run(state: State<'_, RunState>) {
    let pid = {
        let mut inner = lock(&state.inner);
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
            if let Some(ppid) = parent_pid(pid) {
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

    #[test]
    fn lock_carries_on_after_a_panic_while_held() {
        let m = Arc::new(Mutex::new(1));
        let held = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _guard = held.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert!(m.is_poisoned());
        *lock(&m) += 1;
        assert_eq!(*lock(&m), 2);
    }

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
        let prelude = std::env::temp_dir().join(format!("lud-prelude-{}-{}.sh", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
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
        let (child, mut master, out) = on_pty("read -rsp 'Password (hidden): ' p < /dev/tty; echo; echo \"len:${#p}\"");
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
        let (child, mut master, out) = on_pty("stty -echo; printf 'Password: '; builtin read -r p; stty echo; echo; echo \"len:${#p}\"");
        wait_for(&out, "Password: ", 1);
        std::thread::sleep(Duration::from_millis(100));
        let mut prompt =
            Prompt { last_output: Some(Instant::now() - PROMPT_IDLE), open_line: "Password: ".to_string(), ..Prompt::default() };
        assert_eq!(prompt.idle_guess(Some(&master)), Some(("line", true)));
        assert_eq!(prompt.idle_guess(Some(&master)), None, "offered once");
        master.write_all(b"abc\r").unwrap();
        finish(child, &out, "len:3");
        assert!(!text(&out).contains("abc"));
    }

    #[test]
    fn session_password_times_out_only_when_idle() {
        let mut s = Session { password: Some("pw".into()), timeout: Some(Duration::from_secs(60)), idle_since: None };
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
