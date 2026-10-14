//! Install, uninstall and update runs.
//!
//! `linux_util.sh` refuses to run as root and calls `sudo` itself, so it runs
//! as the normal user with a `sudo` shim first on `PATH` that adds `-A`.
//! `SUDO_ASKPASS` points at this same executable started with `--askpass`; it
//! asks the running app for the password over a private Unix socket and the
//! app shows its own dialog. The password is checked once, kept in memory for
//! the length of one run and never written to disk.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

const OPS: [&str; 4] = ["install", "uninstall", "update", "update-all"];
/// The one queue entry an `update-all` run shows.
const UPDATE_ALL_LABEL: &str = "All installed utilities";
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
}

#[derive(Serialize, Clone)]
struct Output<'a> {
    text: &'a str,
    stderr: bool,
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

fn find_in_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&crate::host_var("PATH")?)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
            format!("#!/bin/sh\nexec {} -A \"$@\"\n", shell_quote(&real_sudo.to_string_lossy())),
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
        Ok(work)
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

fn serve_askpass(app: AppHandle, listener: UnixListener, real_sudo: PathBuf, stop: Arc<AtomicBool>) {
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
    let version = std::fs::read_to_string(src.join("linux_util.VERSION")).unwrap_or_default();
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

fn pump<R: Read + Send + 'static>(app: AppHandle, reader: R, stderr: bool) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = clean_line(&buf);
                    let _ = app.emit("run-output", Output { text: &line, stderr });
                }
            }
        }
    })
}

fn run_queue(app: &AppHandle, script: &Path, op: &str, names: &[String], dry_run: bool) {
    let state = app.state::<RunState>();
    let (mut ok, mut failed) = (0, 0);

    let real_sudo = find_in_path("sudo");
    let setup = real_sudo
        .ok_or_else(|| "sudo was not found on PATH".to_string())
        .and_then(|sudo| Workdir::create(&sudo).map(|w| (sudo, w)));
    let (real_sudo, work) = match setup {
        Ok(v) => v,
        Err(e) => {
            let _ = app.emit("run-output", Output { text: &e, stderr: true });
            let _ = app.emit("run-done", Done { ok: 0, failed: names.len(), cancelled: false });
            return;
        }
    };
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
        let mut cmd = crate::script_command(app, script);
        if dry_run {
            cmd.arg("--dry-run");
        }
        cmd.args(["--no-color", &format!("--{op}")]);
        if op != "update-all" {
            cmd.arg(name);
        }
        cmd.env("PATH", &path)
            .env("SUDO_ASKPASS", work.askpass_path())
            .env(SOCKET_ENV, work.socket_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("Could not start linux_util.sh: {e}");
                let _ = app.emit("run-output", Output { text: &msg, stderr: true });
                let _ = app.emit("run-item", ItemEvent { index, name, state: "failed", code: None });
                failed += 1;
                continue;
            }
        };
        state.inner.lock().unwrap().pid = Some(child.id());
        let readers = [
            pump(app.clone(), child.stdout.take().unwrap(), false),
            pump(app.clone(), child.stderr.take().unwrap(), true),
        ];
        let status = child.wait();
        for r in readers {
            let _ = r.join();
        }
        let cancelled = {
            let mut inner = state.inner.lock().unwrap();
            inner.pid = None;
            inner.cancelled
        };
        let code = status.as_ref().ok().and_then(|s| s.code());
        let item_state = if cancelled {
            "cancelled"
        } else if code == Some(0) {
            ok += 1;
            "ok"
        } else {
            failed += 1;
            "failed"
        };
        let _ = app.emit("run-item", ItemEvent { index, name, state: item_state, code });
    }

    stop.store(true, Ordering::SeqCst);
    if let Some(s) = server {
        let _ = s.join();
    }
    let cancelled = state.inner.lock().unwrap().cancelled;
    let _ = app.emit("run-done", Done { ok, failed, cancelled });
}

/// Starts a queue of `op` runs, one utility at a time. Progress arrives as the
/// `run-item`, `run-output`, `askpass-request` and `run-done` events.
/// `update-all` takes no names: it is one `--update-all` run.
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
    let names = if op == "update-all" { vec![UPDATE_ALL_LABEL.to_string()] } else { names };
    if names.is_empty() {
        return Err("Nothing selected".to_string());
    }
    let script = staged_script(&app, &crate::script_path(&app)?)?;
    {
        let mut inner = state.inner.lock().unwrap();
        if inner.running {
            return Err("Another action is already running".to_string());
        }
        *inner = Inner { running: true, ..Inner::default() };
    }
    std::thread::spawn(move || {
        run_queue(&app, &script, &op, &names, dry_run);
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
    unsafe { libc::kill(group, libc::SIGTERM) };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        // Signal 0 only checks that the group still exists.
        if unsafe { libc::kill(group, 0) } == 0 {
            unsafe { libc::kill(group, libc::SIGKILL) };
        }
    });
}
