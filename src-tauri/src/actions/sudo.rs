//! The sudo password: asked for once, held for the app session, and handed
//! over the askpass socket only to `sudo` started by the app's own runs.

use super::*;

#[derive(Serialize, Clone)]
pub(super) struct AskpassRequest {
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

/// True when `password` is accepted by sudo. Ignores any cached credentials.
pub(super) fn password_ok(real_sudo: &Path, password: &str) -> bool {
    let mut cmd = Command::new(real_sudo);
    crate::use_host_env(&mut cmd);
    let Ok(mut child) = cmd.args(["-S", "-k", "-v", "-p", ""]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
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
///
/// Checked with `sudo -v`, which linux_util runs at the start of every run,
/// rather than with a command: under sudo's default `verifypw=all`, `-v` still
/// wants a password when any of the user's rules lacks NOPASSWD. Fedora's WSL
/// image gives its first user both `%wheel ALL=(ALL) ALL` and a NOPASSWD rule,
/// so commands run without a password but `sudo -v` asks for one.
pub(super) fn sudo_without_password(real_sudo: &Path) -> bool {
    let mut cmd = Command::new(real_sudo);
    crate::use_host_env(&mut cmd);
    cmd.args(["-n", "-k", "-v"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    matches!(cmd.status(), Ok(s) if s.success())
}

/// sudo's default `timestamp_timeout`, used when the configured one can't be read.
pub(super) const SUDO_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// sudo's `timestamp_timeout`, as `sudo -V` reports it when run as root
/// ("Authentication timestamp timeout: 5.0 minutes"). `None` means never time
/// out (a negative setting). Falls back to sudo's default of 5 minutes.
pub(super) fn sudo_timeout(real_sudo: &Path, password: &str) -> Option<Duration> {
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

pub(super) fn parse_sudo_timeout(sudo_v: &str) -> Option<Duration> {
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
pub(super) fn ask_password(app: &AppHandle, real_sudo: &Path) -> Option<String> {
    let state = app.state::<RunState>();
    let mut error = None;
    let answer = loop {
        let (tx, rx) = mpsc::channel();
        lock(&state.inner).reply = Some(tx);
        let _ = app.emit("askpass-request", AskpassRequest { error: error.take() });
        match rx.recv() {
            Ok(Some(pw)) if password_ok(real_sudo, &pw) => break Some(pw),
            Ok(Some(_)) => error = Some("Wrong password, try again.".to_string()),
            _ => break None,
        }
    };
    lock(&state.inner).reply = None;
    let _ = app.emit("askpass-close", ());
    answer
}

/// Holds an accepted password for the session. Outside a run, its timeout
/// starts now.
pub(super) fn keep_password(state: &RunState, real_sudo: &Path, password: String) {
    let timeout = sudo_timeout(real_sudo, &password);
    let running = lock(&state.inner).running;
    let mut session = lock(&state.session);
    session.password = Some(password);
    session.timeout = timeout;
    session.idle_since = if running { None } else { Some(Instant::now()) };
}

/// The session's password, or the one `ask` returns, which is then kept for
/// the session. The session lock is let go before asking: `keep_password`
/// takes it again, and a `Mutex` locked twice by one thread never returns.
pub(super) fn held_or_ask(state: &RunState, real_sudo: &Path, ask: impl FnOnce() -> Option<String>) -> Option<String> {
    let held = lock(&state.session).password();
    held.or_else(|| {
        let pw = ask()?;
        keep_password(state, real_sudo, pw.clone());
        Some(pw)
    })
}

/// The pid and uid of the process that connected `stream`, from the kernel.
pub(super) fn peer_cred(stream: &UnixStream) -> Option<(u32, u32)> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len)
    };
    (rc == 0 && cred.pid > 0).then_some((cred.pid as u32, cred.uid))
}

/// The parent of `pid`, from `/proc/<pid>/stat`.
pub(super) fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name in parentheses may hold spaces; the parent pid is the
    // second field after its closing bracket.
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

/// True when `pid` is a child, grandchild and so on of `ancestor`.
pub(super) fn descends_from(pid: u32, ancestor: u32) -> bool {
    let mut current = pid;
    // Bounded, in case /proc changes under us and a loop appears.
    for _ in 0..4096 {
        match parent_pid(current) {
            Some(p) if p == ancestor => return true,
            Some(p) if p > 1 => current = p,
            _ => return false,
        }
    }
    false
}

/// Answers one askpass connection: with the password when the client is ours
/// (same user, and started from `root`, so a step of the run), with nothing
/// otherwise. `password` gives the session's password, asking if needed, and
/// is only called for our own clients. Returns false when it gave none (the
/// dialog was cancelled), which denies the rest of the run; a stray client
/// never does, so it cannot block the real sudo.
pub(super) fn answer_askpass(stream: &mut UnixStream, root: u32, password: &mut dyn FnMut() -> Option<String>) -> bool {
    let ours = peer_cred(stream).is_some_and(|(pid, uid)| uid == unsafe { libc::getuid() } && descends_from(pid, root));
    if !ours {
        return true; // dropping the stream makes that askpass exit 1
    }
    match password() {
        Some(pw) => {
            let _ = stream.write_all(pw.as_bytes());
            true
        }
        None => false,
    }
}

/// Answers each askpass request from the session's password, asking for it
/// (once) when none is held. After a cancel, the rest of the run's requests
/// fail without asking again. Only processes the app started are answered:
/// the socket's 0700 directory keeps other users out, and this keeps out
/// other programs of the same user.
pub(super) fn serve_askpass(app: AppHandle, listener: UnixListener, real_sudo: PathBuf, stop: Arc<AtomicBool>) {
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
        denied =
            !answer_askpass(&mut stream, std::process::id(), &mut || held_or_ask(&state, &real_sudo, || ask_password(&app, &real_sudo)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected pair on a fresh socket path; the client end is this
    /// process, so its peer pid is our own.
    fn socket_pair(tag: &str) -> (UnixStream, UnixStream, PathBuf) {
        let path = std::env::temp_dir().join(format!("lud-askpass-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        (server, client, path)
    }

    #[test]
    fn askpass_peer_is_read_from_the_socket() {
        let (server, _client, path) = socket_pair("cred");
        let (pid, uid) = peer_cred(&server).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(pid, std::process::id());
        assert_eq!(uid, unsafe { libc::getuid() });
    }

    #[test]
    fn descent_is_followed_through_parents() {
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        let me = std::process::id();
        assert!(descends_from(child.id(), me));
        assert!(descends_from(me, std::os::unix::process::parent_id()));
        assert!(!descends_from(me, me), "a process is not its own descendant");
        assert!(!descends_from(me, child.id()));
        assert!(!descends_from(1, me));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn askpass_answers_only_the_runs_own_processes() {
        let parent = std::os::unix::process::parent_id();
        let me = std::process::id();
        // Asked from inside the run's tree: the password is sent.
        let (mut server, mut client, path) = socket_pair("ok");
        let mut asked = 0;
        assert!(answer_askpass(&mut server, parent, &mut || {
            asked += 1;
            Some("pw".to_string())
        }));
        drop(server);
        let mut got = String::new();
        client.read_to_string(&mut got).unwrap();
        assert_eq!((got.as_str(), asked), ("pw", 1));
        std::fs::remove_file(path).unwrap();

        // Asked from outside it: nothing is sent, the password is never
        // looked up, and the run is not marked denied.
        let (mut server, mut client, path) = socket_pair("stray");
        assert!(answer_askpass(&mut server, me, &mut || panic!("a stray client must not reach the password")));
        drop(server);
        let mut got = String::new();
        client.read_to_string(&mut got).unwrap();
        assert_eq!(got, "");
        std::fs::remove_file(path).unwrap();

        // A cancelled dialog denies the run.
        let (mut server, _client, path) = socket_pair("cancel");
        assert!(!answer_askpass(&mut server, parent, &mut || None));
        std::fs::remove_file(path).unwrap();
    }

    /// Real sudo asking through the real askpass client. Needs a sudo that
    /// wants a password, so it runs only when asked, in a throwaway container:
    /// `LUD_APP_BIN=<built app> LUD_SUDO_TEST_PASSWORD=<that user's password>
    /// cargo test --lib real_sudo -- --ignored`
    #[test]
    #[ignore]
    fn real_sudo_gets_the_password_only_inside_the_run() {
        let app_bin = std::env::var("LUD_APP_BIN").expect("LUD_APP_BIN");
        let password = std::env::var("LUD_SUDO_TEST_PASSWORD").expect("LUD_SUDO_TEST_PASSWORD");
        let dir = std::env::temp_dir().join(format!("lud-real-sudo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let askpass = dir.join("askpass");
        std::fs::write(&askpass, format!("#!/bin/sh\nexec {} --askpass \"$@\"\n", shell_quote(&app_bin))).unwrap();
        std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let answered = Arc::new(Mutex::new(0));
        let server = {
            let (stop, answered) = (stop.clone(), answered.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    };
                    stream.set_nonblocking(false).unwrap();
                    answer_askpass(&mut stream, std::process::id(), &mut || {
                        *lock(&answered) += 1;
                        Some(password.clone())
                    });
                }
            })
        };
        let run = |script: &str| {
            Command::new("sh").arg("-c").arg(script).env("SUDO_ASKPASS", &askpass).env(SOCKET_ENV, &socket).status().unwrap().success()
        };
        // `-k` drops cached credentials, so every one of these must ask.
        assert!(run("sudo -k -A true"), "direct");
        assert!(run("timeout 20 sudo -k -A true"), "inside timeout");
        assert!(run("bash -c 'bash -c \"sudo -k -A true\"'"), "nested scripts");
        // Left running after its parent exits, so no longer in the tree.
        let result = dir.join("stray");
        assert!(run(&format!("(sleep 1; sudo -k -A true; echo $? > {}) & exit 0", shell_quote(&result.to_string_lossy()))));
        let started = Instant::now();
        while !result.exists() && started.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(100));
        let stray = std::fs::read_to_string(&result).unwrap();
        assert_ne!(stray.trim(), "0", "a process outside the run got the password");
        assert!(run("sudo -k -A true"), "a real request after the stray one");
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(*lock(&answered), 4, "only the four requests from inside the run were answered");
    }

    /// A password first asked for during a run (none held: the start-up dialog
    /// was skipped or cancelled, or the held one timed out) is handed over and
    /// kept. Holding the session lock while asking deadlocked here: the dialog
    /// stayed on "Checking…" and the run never ended (#14).
    #[test]
    fn password_asked_during_a_run_is_returned_and_kept() {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let state = RunState::default();
            // No sudo there, so keep_password falls back to sudo's default timeout.
            let missing = Path::new("/nonexistent/sudo");
            let first = held_or_ask(&state, missing, || Some("pw".to_string()));
            let second = held_or_ask(&state, missing, || panic!("a held password must not be asked for again"));
            let _ = tx.send((first, second));
        });
        let (first, second) = rx.recv_timeout(Duration::from_secs(5)).expect("asking for the password during a run hung");
        assert_eq!(first.as_deref(), Some("pw"));
        assert_eq!(second.as_deref(), Some("pw"));
    }

    /// The start-up check against a real sudo. Needs a prepared sudoers, so it
    /// runs only when asked, in a throwaway container, with what the check
    /// should say for that setup:
    /// `LUD_EXPECT_NOPASSWD=yes|no cargo test --lib start_up_check -- --ignored`
    #[test]
    #[ignore]
    fn start_up_check_matches_sudo() {
        let expect = match std::env::var("LUD_EXPECT_NOPASSWD").expect("LUD_EXPECT_NOPASSWD").as_str() {
            "yes" => true,
            "no" => false,
            other => panic!("LUD_EXPECT_NOPASSWD must be yes or no, not {other}"),
        };
        let sudo = find_in_path("sudo").expect("sudo on PATH");
        assert_eq!(sudo_without_password(&sudo), expect);
    }

    #[test]
    fn sudo_timeout_is_read_from_sudo_v() {
        let v = "Sudo version 1.9.17\nSudoers policy plugin version 1.9.17\n  Authentication timestamp timeout: 15.0 minutes\n";
        assert_eq!(parse_sudo_timeout(v), Some(Duration::from_secs(15 * 60)));
        assert_eq!(parse_sudo_timeout("Authentication timestamp timeout: 0.0 minutes"), Some(Duration::ZERO));
        assert_eq!(parse_sudo_timeout("Authentication timestamp timeout: -1.0 minutes"), None);
        assert_eq!(parse_sudo_timeout("Sudo version 1.9.17"), Some(SUDO_DEFAULT_TIMEOUT));
    }
}
