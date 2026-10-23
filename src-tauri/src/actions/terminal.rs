//! What each script runs in: its own pseudo-terminal, and a private work
//! folder holding the `sudo` shim and the `read` wrapper.

use super::*;

/// The `sudo` the script runs: the real sudo with `-A`, so the password comes
/// from the app's dialog. linux_util itself skips restarting xrdp while RDP
/// sessions are open (docs/CONTRACT.md), so nothing is held back here.
pub(super) const SUDO_SHIM: &str = r#"#!/bin/sh
# sudo drops exported functions, so a script run as `sudo bash <script>` (the
# guest tools ISO's installer) would lose its `read` wrapper; load the app's
# inside root's bash. linux_util's own `sudo bash` handling loads its wrapper
# after this one, which then replaces it.
if [ "$1" = bash ]; then
    shift
    exec {REAL_SUDO} -A bash -c '. {PRELUDE}; export -f read; exec bash "$@"' bash "$@"
fi
exec {REAL_SUDO} -A "$@"
"#;

/// The app's own `read` wrapper. It changes nothing about what `read` does; it
/// only lets the app follow it. On a terminal, linux_util sends the same
/// signals itself (`LINUX_UTIL_GUI=1`), so this is loaded through `BASH_ENV`
/// only for a run without a terminal, and by `SUDO_SHIM` for `sudo bash`.
///
/// On a terminal it tells the run panel that a question is waiting and
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
pub(super) const READ_PRELUDE: &str = r#"read() {
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
pub(super) struct Pty {
    pub(super) master: File,
    pub(super) slave: File,
}

/// The terminal size the script is told. Programs that draw to the width (dnf
/// and apt progress bars, tables) lay out for this many columns.
pub(super) const PTY_COLS: u16 = 120;
pub(super) const PTY_ROWS: u16 = 40;

impl Pty {
    pub(super) fn open() -> std::io::Result<Self> {
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
pub(super) struct Workdir {
    root: PathBuf,
}

impl Workdir {
    pub(super) fn create(real_sudo: &Path) -> Result<Self, String> {
        let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("linux-util-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |p: &Path| std::fs::DirBuilder::new().mode(0o700).create(p).map_err(|e| format!("Could not create {}: {e}", p.display()));
        mk(&root)?;
        let work = Workdir { root };
        mk(&work.bin_dir())?;

        let exe = std::env::current_exe().map_err(|e| format!("Cannot find own executable: {e}"))?;
        let write_exec = |p: PathBuf, body: String| -> Result<(), String> {
            std::fs::write(&p, body).map_err(|e| format!("Could not write {}: {e}", p.display()))?;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
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
        write_exec(work.askpass_path(), format!("#!/bin/sh\n{libs}exec {} --askpass \"$@\"\n", shell_quote(&exe.to_string_lossy())))?;
        std::fs::write(work.prelude_path(), READ_PRELUDE).map_err(|e| format!("Could not write {}: {e}", work.prelude_path().display()))?;
        Ok(work)
    }
    pub(super) fn prelude_path(&self) -> PathBuf {
        self.root.join("prelude.sh")
    }
    pub(super) fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }
    pub(super) fn askpass_path(&self) -> PathBuf {
        self.root.join("askpass")
    }
    pub(super) fn socket_path(&self) -> PathBuf {
        self.root.join("s")
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
