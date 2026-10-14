mod actions;

use std::path::PathBuf;
use std::process::Command;

use tauri::Manager;

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

/// Runs `linux_util.sh --list --json --no-color` and returns the JSON document.
/// Anything printed before the first `{` (such as a `Detected:` line) is dropped.
#[tauri::command]
async fn run_list(app: tauri::AppHandle) -> Result<String, String> {
    let script = script_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let output = Command::new("bash")
            .arg(&script)
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
        .manage(actions::RunState::default())
        .invoke_handler(tauri::generate_handler![
            run_list,
            detected_distro,
            actions::run_action,
            actions::askpass_reply,
            actions::cancel_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running Linux_Util Desktop");
}
