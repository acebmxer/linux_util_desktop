//! Profiles: named lists of utilities in linux_util's profile export format.
//!
//! The built-in profiles come from `linux_util.sh --export-profile`. Import and
//! export use the same JSON format, so a file saved here can be applied on
//! another machine with `linux_util.sh --import-profile`. The file dialogs open
//! from Rust, so the window never handles file paths itself.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

/// Larger than any real profile file; anything bigger is not one.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub struct Profile {
    label: String,
    #[serde(default)]
    description: String,
    utilities: Vec<String>,
}

/// linux_util's export format. Field order matches `profile_export`, and
/// `utilities` stays last in each profile: `--import-profile` reads every
/// quoted string after the first `"utilities"` line as a utility name.
#[derive(Serialize, Deserialize)]
struct ProfileFile {
    linux_util_profile_export: bool,
    version: u32,
    #[serde(default)]
    exported_at: String,
    profiles: Vec<Profile>,
}

#[derive(Serialize)]
pub struct Imported {
    file: String,
    profiles: Vec<Profile>,
}

fn parse(text: &str) -> Result<Vec<Profile>, String> {
    let doc: ProfileFile = serde_json::from_str(text).map_err(|_| "is not a linux_util profile file".to_string())?;
    if !doc.linux_util_profile_export {
        return Err("is not a linux_util profile file".to_string());
    }
    if doc.version != 1 {
        return Err(format!("is profile format version {}; this app reads version 1", doc.version));
    }
    Ok(doc.profiles)
}

fn read_limited(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?.len();
    if size > MAX_FILE_BYTES {
        return Err(format!("{} is too large to be a profile file", path.display()));
    }
    std::fs::read_to_string(path).map_err(|e| format!("Could not read {}: {e}", path.display()))
}

/// The current UTC time as `YYYY-MM-DDTHH:MM:SSZ`, the format `profile_export` writes.
fn utc_timestamp() -> String {
    format_utc(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64)
}

fn format_utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// The profiles built into the bundled linux_util, via `--export-profile`.
#[tauri::command]
pub async fn list_profiles(app: AppHandle) -> Result<Vec<Profile>, String> {
    let mut cmd = crate::script_command(&app, &crate::script_path(&app)?);
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("No app cache directory: {e}"))?;
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        let out = dir.join(format!("profiles-{}.json", std::process::id()));
        let output = cmd
            .args(["--no-color", "--export-profile"])
            .arg(&out)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("Could not start linux_util.sh: {e}"))?;
        let text = read_limited(&out);
        let _ = std::fs::remove_file(&out);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("linux_util.sh --export-profile failed ({}): {}", output.status, stderr.trim()));
        }
        parse(&text?).map_err(|e| format!("linux_util.sh --export-profile output {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A file dialog that starts in the home directory. Without one it starts in
/// the working directory, which is `/` from a desktop menu.
fn file_dialog(app: &AppHandle) -> tauri_plugin_dialog::FileDialogBuilder<tauri::Wry> {
    let dialog = app.dialog().file();
    match app.path().home_dir() {
        Ok(home) => dialog.set_directory(home),
        Err(_) => dialog,
    }
}

/// Asks for a profile file and returns its profiles; `None` if the user cancelled.
#[tauri::command]
pub async fn import_profile(app: AppHandle) -> Result<Option<Imported>, String> {
    let Some(picked) = file_dialog(&app)
        .set_title("Import profile")
        .add_filter("Profile (JSON)", &["json"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let profiles = parse(&read_limited(&path)?).map_err(|e| format!("{file} {e}"))?;
    if profiles.is_empty() {
        return Err(format!("{file} has no profiles in it"));
    }
    Ok(Some(Imported { file, profiles }))
}

/// Asks where to save, then writes `utilities` as a one-profile file named
/// `label`. Returns the file name, or `None` if the user cancelled.
#[tauri::command]
pub async fn export_profile(app: AppHandle, label: String, utilities: Vec<String>) -> Result<Option<String>, String> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("The profile needs a name".to_string());
    }
    if utilities.is_empty() {
        return Err("Nothing selected to export".to_string());
    }
    let suggested: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let Some(picked) = file_dialog(&app)
        .set_title("Export profile")
        .set_file_name(format!("{suggested}.json"))
        .add_filter("Profile (JSON)", &["json"])
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    let doc = ProfileFile {
        linux_util_profile_export: true,
        version: 1,
        exported_at: utc_timestamp(),
        profiles: vec![Profile { label, description: String::new(), utilities }],
    };
    let mut text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(Some(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linux_util_export() {
        let text = r#"{
  "linux_util_profile_export": true,
  "version": 1,
  "exported_at": "2026-10-14T14:36:08Z",
  "profiles": [
    {
      "label": "Home Desktop",
      "description": "Home desktop.",
      "utilities": ["Firefox", "Btop"]
    }
  ]
}"#;
        let profiles = parse(text).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].label, "Home Desktop");
        assert_eq!(profiles[0].utilities, ["Firefox", "Btop"]);
    }

    #[test]
    fn rejects_other_json() {
        assert!(parse(r#"{"schema": "linux_util/list/v1", "utilities": []}"#).is_err());
        assert!(parse(r#"{"linux_util_profile_export": false, "version": 1, "profiles": []}"#).is_err());
        assert!(parse(r#"{"linux_util_profile_export": true, "version": 2, "profiles": []}"#).is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn export_keeps_linux_util_layout() {
        let doc = ProfileFile {
            linux_util_profile_export: true,
            version: 1,
            exported_at: utc_timestamp(),
            profiles: vec![Profile { label: "Mine".into(), description: String::new(), utilities: vec!["Btop".into()] }],
        };
        let text = serde_json::to_string_pretty(&doc).unwrap();
        // `--import-profile` greps for this exact text.
        assert!(text.contains(r#""linux_util_profile_export": true"#));
        let utilities = text.find("\"utilities\"").unwrap();
        assert!(text.find("\"label\"").unwrap() < utilities);
        assert!(text.find("\"description\"").unwrap() < utilities);
    }

    #[test]
    fn timestamp_format() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_791_988_568), "2026-10-14T14:36:08Z");
    }
}
