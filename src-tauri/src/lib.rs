mod actions;
mod logs;
mod profiles;
mod settings;
mod update;

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::process::Command;

use tauri::Manager;

/// The AppImage's unpacked bundle directory, when running from an AppImage.
fn appimage_dir() -> Option<OsString> {
    std::env::var_os("APPIMAGE")?;
    std::env::var_os("APPDIR").filter(|d| d.len() > 1)
}

/// `value` without the `:`-separated entries inside `appdir`; `None` when
/// nothing is left. Values that never mention `appdir` come back unchanged.
fn strip_appdir(value: &OsStr, appdir: &OsStr) -> Option<OsString> {
    let (bytes, dir) = (value.as_bytes(), appdir.as_bytes());
    if !bytes.windows(dir.len()).any(|w| w == dir) {
        return Some(value.to_owned());
    }
    let inside = |e: &[u8]| e == dir || (e.starts_with(dir) && e.get(dir.len()) == Some(&b'/'));
    let kept: Vec<&[u8]> = bytes.split(|b| *b == b':').filter(|e| !e.is_empty() && !inside(e)).collect();
    (!kept.is_empty()).then(|| OsString::from_vec(kept.join(&b':')))
}

/// An environment variable as the host set it: under an AppImage, without the
/// entries its launcher added inside the bundle.
pub(crate) fn host_var(key: &str) -> Option<OsString> {
    let value = std::env::var_os(key)?;
    match appimage_dir() {
        Some(dir) => strip_appdir(&value, &dir),
        None => Some(value),
    }
}

/// The AppImage launcher points `PATH`, `LD_LIBRARY_PATH`, `XDG_DATA_DIRS`,
/// the GTK and GIO module paths and more into the bundle, and forces
/// `GTK_THEME`. Programs that linux_util.sh starts need the host's own, or they
/// load the bundle's older libraries and fail (curl, git and pacman: "symbol
/// lookup error"). It also starts the app inside the bundle, so `cmd` runs from
/// the home directory instead. Outside an AppImage this changes nothing.
pub(crate) fn use_host_env(cmd: &mut Command) {
    let Some(dir) = appimage_dir() else { return };
    if let Some(home) = std::env::var_os("HOME").filter(|h| std::path::Path::new(h).is_dir()) {
        cmd.current_dir(home);
    }
    for (key, value) in std::env::vars_os() {
        match strip_appdir(&value, &dir) {
            Some(kept) if kept == value => {}
            Some(kept) => {
                cmd.env(key, kept);
            }
            None => {
                cmd.env_remove(key);
            }
        }
    }
    cmd.env_remove("GTK_THEME");
}

/// Finds the bundled `linux_util.sh`: the packaged resource directory first,
/// then the repo's `vendor/` directory when running from a source checkout.
pub(crate) fn script_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join("vendor").join("linux_util.sh"));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("vendor")
            .join("linux_util.sh"),
    );
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| "Bundled linux_util.sh not found".to_string())
}

/// `bash <script>` with the host's environment (see `use_host_env`) and the
/// app's linux_util config file. Every run of linux_util.sh starts here.
pub(crate) fn script_command(app: &tauri::AppHandle, script: &std::path::Path) -> Command {
    let mut cmd = Command::new("bash");
    use_host_env(&mut cmd);
    if let Some(conf) = settings::linux_util_conf_path(app) {
        cmd.env("LINUX_UTIL_CONFIG_FILE", conf);
    }
    cmd.arg(script);
    cmd
}

/// Runs `linux_util.sh --list --json --no-color` and returns the JSON document.
/// Anything printed before the first `{` (such as a `Detected:` line) is dropped.
#[tauri::command]
async fn run_list(app: tauri::AppHandle) -> Result<String, String> {
    let mut cmd = script_command(&app, &script_path(&app)?);
    tauri::async_runtime::spawn_blocking(move || {
        let output = cmd
            .args(["--list", "--json", "--no-color"])
            .output()
            .map_err(|e| format!("Could not start linux_util.sh: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "linux_util.sh --list failed ({}): {}",
                output.status,
                stderr.trim()
            ));
        }
        match stdout.find('{') {
            Some(start) => Ok(stdout[start..].to_string()),
            None => Err("linux_util.sh --list produced no JSON".to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The distro's display name from `/etc/os-release` (falls back to `/usr/lib`).
#[tauri::command]
fn detected_distro() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

/// Entry point for `SUDO_ASKPASS` (`--askpass`): no window, just print the password.
pub fn askpass() -> ! {
    actions::askpass_client()
}

pub fn run() {
    // WebKitGTK's DMA-BUF renderer crashes the window on some Wayland + NVIDIA
    // setups ("Error 71 (Protocol error) dispatching to Wayland display").
    // Respect an explicit setting from the user's environment.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(actions::RunState::default())
        .invoke_handler(tauri::generate_handler![
            run_list,
            detected_distro,
            actions::run_action,
            actions::askpass_reply,
            actions::cancel_run,
            profiles::list_profiles,
            profiles::import_profile,
            profiles::export_profile,
            settings::get_settings,
            settings::save_settings,
            logs::list_logs,
            logs::read_log,
            update::version_info,
            update::check_update,
            update::install_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running Linux_Util Desktop");
}
