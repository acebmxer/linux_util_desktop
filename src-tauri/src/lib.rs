mod actions;
mod logs;
mod profiles;
mod settings;
mod update;
mod window_state;

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

/// `linux_util.sh --list --json` carries no descriptions, but linux_util keeps
/// one per utility as `UTILITY_DESCRIPTION["Name"]="text"` in `lib/installers.sh`.
/// Reads those lines into a name -> text map. Only the escapes bash applies
/// inside double quotes (`\"`, `\$`, `\\`, `\``) are undone. Delete this once
/// linux_util puts `description` in the list JSON itself.
fn read_descriptions(script: &std::path::Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(dir) = script.parent() else { return map };
    let Ok(text) = std::fs::read_to_string(dir.join("lib").join("installers.sh")) else {
        return map;
    };
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("UTILITY_DESCRIPTION[\"") else { continue };
        let Some((name, rest)) = rest.split_once("\"]=\"") else { continue };
        let Some(body) = rest.strip_suffix('"') else { continue };
        let mut out = String::with_capacity(body.len());
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(n @ ('"' | '$' | '\\' | '`')) => out.push(n),
                    Some(n) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        map.insert(name.to_string(), out);
    }
    map
}

/// Adds a `description` to each utility in the list JSON. Returns the JSON
/// unchanged if it can't be parsed, so a list never fails over this.
fn with_descriptions(json: &str, script: &std::path::Path) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    let descriptions = read_descriptions(script);
    if let Some(items) = doc.get_mut("utilities").and_then(|u| u.as_array_mut()) {
        for item in items {
            let found = item
                .get("name")
                .and_then(|n| n.as_str())
                .and_then(|n| descriptions.get(n))
                .cloned();
            if let (Some(text), Some(obj)) = (found, item.as_object_mut()) {
                obj.insert("description".into(), text.into());
            }
        }
    }
    doc.to_string()
}

/// The menu's own tab order: `CATEGORIES=(...)` and the per-category
/// `SUBCATEGORY_ORDER["Category"]="A|B"` lines in `lib/installers.sh`. The list
/// JSON carries neither, and the desktop app must show the same order as the
/// menu. Missing or unreadable lines give empty results, never an error.
fn read_menu_order(script: &std::path::Path) -> (Vec<String>, serde_json::Map<String, serde_json::Value>) {
    let mut categories = Vec::new();
    let mut subs = serde_json::Map::new();
    let Some(dir) = script.parent() else { return (categories, subs) };
    let Ok(text) = std::fs::read_to_string(dir.join("lib").join("installers.sh")) else {
        return (categories, subs);
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("CATEGORIES=(") {
            let body = rest.split(')').next().unwrap_or("");
            categories = body.split('"').skip(1).step_by(2).map(str::to_string).collect();
        } else if let Some(rest) = line.strip_prefix("SUBCATEGORY_ORDER[\"") {
            let Some((name, rest)) = rest.split_once("\"]=\"") else { continue };
            let Some(body) = rest.strip_suffix('"') else { continue };
            let order: Vec<&str> = body.split('|').collect();
            subs.insert(name.to_string(), serde_json::json!(order));
        }
    }
    (categories, subs)
}

/// Adds `category_order` and `subcategory_order` to the list JSON. Returns the
/// JSON unchanged if it can't be parsed. Delete this once linux_util puts them
/// in the list JSON itself.
fn with_menu_order(json: &str, script: &std::path::Path) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    let (categories, subs) = read_menu_order(script);
    if let Some(obj) = doc.as_object_mut() {
        obj.insert("category_order".into(), serde_json::json!(categories));
        obj.insert("subcategory_order".into(), subs.into());
    }
    doc.to_string()
}

/// Runs `linux_util.sh --list --json --no-color` and returns the JSON document.
/// Anything printed before the first `{` (such as a `Detected:` line) is dropped.
#[tauri::command]
async fn run_list(app: tauri::AppHandle) -> Result<String, String> {
    let script = script_path(&app)?;
    let mut cmd = script_command(&app, &script);
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
            Some(start) => Ok(with_menu_order(&with_descriptions(&stdout[start..], &script), &script)),
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

/// True when running inside WSL (WSL 1 and 2 both set `WSL_DISTRO_NAME`).
#[cfg(target_os = "linux")]
fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|v| v.to_lowercase().contains("microsoft"))
            .unwrap_or(false)
}

pub fn run() {
    // On Wayland, KDE finds the window's icon by matching its application ID to
    // a .desktop file name. GTK uses the program name as the ID, and the
    // package installs "Linux_Util Desktop.desktop", so the two have to match.
    #[cfg(target_os = "linux")]
    glib::set_prgname(Some("Linux_Util Desktop"));

    // WebKitGTK's DMA-BUF renderer crashes the window on some Wayland + NVIDIA
    // setups ("Error 71 (Protocol error) dispatching to Wayland display").
    // Respect an explicit setting from the user's environment.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    // Under WSLg, WebKitGTK crashes (segmentation fault) when it renders through
    // the "dzn" Vulkan-over-DirectX driver, so on WSL render in software.
    // Respect explicit settings from the user's environment.
    #[cfg(target_os = "linux")]
    if is_wsl() {
        for var in ["LIBGL_ALWAYS_SOFTWARE", "WEBKIT_DISABLE_COMPOSITING_MODE"] {
            if std::env::var_os(var).is_none() {
                std::env::set_var(var, "1");
            }
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(actions::RunState::default())
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                window_state::restore(&window);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(w) = window.app_handle().get_webview_window(window.label()) {
                    window_state::save(&w);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            run_list,
            detected_distro,
            actions::run_action,
            actions::askpass_reply,
            actions::cancel_run,
            actions::send_input,
            actions::reboot_needed,
            actions::reboot_now,
            actions::forget_password,
            profiles::list_profiles,
            profiles::import_profile,
            profiles::export_profile,
            settings::get_settings,
            window_state::get_sidebar_width,
            window_state::set_sidebar_width,
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
