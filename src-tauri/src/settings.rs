//! App settings: the theme, plus the linux_util config file the app hands to
//! linux_util.sh through `LINUX_UTIL_CONFIG_FILE`.
//!
//! Both live in the app config directory. `settings.json` is the app's own
//! record; `linux_util.conf` is generated from it in linux_util's `key=value`
//! format, holding only the keys changed from linux_util's defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const THEMES: [&str; 3] = ["system", "light", "dark"];

/// The linux_util config keys the app offers: only those its `--install`,
/// `--uninstall`, `--update` and `--update-all` runs actually read. The rest
/// (retries, pre-flight checks) belong to the terminal menu.
const KEYS: [(&str, Kind); 5] = [
    ("auto_confirm", Kind::Bool),
    ("verbose", Kind::Bool),
    ("debug", Kind::Bool),
    ("log_retention_days", Kind::Int),
    ("max_logs_per_day", Kind::Int),
];

#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Int,
}

#[derive(Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_theme")]
    theme: String,
    /// linux_util config keys changed from their defaults.
    #[serde(default)]
    linux_util: BTreeMap<String, String>,
}

fn default_theme() -> String {
    "system".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings { theme: default_theme(), linux_util: BTreeMap::new() }
    }
}

impl Settings {
    fn validate(&self) -> Result<(), String> {
        if !THEMES.contains(&self.theme.as_str()) {
            return Err(format!("Unknown theme: {}", self.theme));
        }
        for (key, value) in &self.linux_util {
            let Some((_, kind)) = KEYS.iter().find(|(k, _)| k == key) else {
                return Err(format!("Unknown linux_util setting: {key}"));
            };
            let ok = match kind {
                Kind::Bool => value == "true" || value == "false",
                Kind::Int => !value.is_empty() && value.len() <= 6 && value.bytes().all(|b| b.is_ascii_digit()),
            };
            if !ok {
                return Err(format!("Invalid value for {key}: {value}"));
            }
        }
        Ok(())
    }

    /// The `linux_util.conf` text, or `None` when every key is at its default.
    fn linux_util_conf(&self) -> Option<String> {
        if self.linux_util.is_empty() {
            return None;
        }
        let mut text = String::from("# Written by Linux_Util Desktop from its Settings window. Changes here are overwritten.\n");
        for (key, value) in &self.linux_util {
            text.push_str(&format!("{key}={value}\n"));
        }
        Some(text)
    }
}

fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_config_dir().map_err(|e| format!("No app config directory: {e}"))
}

/// Where the app's linux_util config lives. linux_util.sh is always pointed
/// here, so a stray `linux_util.conf` next to the bundled script is never read.
pub(crate) fn linux_util_conf_path(app: &AppHandle) -> Option<PathBuf> {
    config_dir(app).ok().map(|d| d.join("linux_util.conf"))
}

/// Writes `text` to `path` through a temporary file, so a crash never leaves
/// half a file behind.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| format!("Could not write {}: {e}", path.display()))
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    let Ok(dir) = config_dir(&app) else { return Settings::default() };
    std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
        .filter(|s| s.validate().is_ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    settings.validate()?;
    let dir = config_dir(&app)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("settings.json"), &json)?;
    let conf = dir.join("linux_util.conf");
    match settings.linux_util_conf() {
        Some(text) => write_atomic(&conf, &text),
        None => match std::fs::remove_file(&conf) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("Could not remove {}: {e}", conf.display()))
            }
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(pairs: &[(&str, &str)]) -> Settings {
        Settings {
            theme: "dark".into(),
            linux_util: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn accepts_known_keys() {
        assert!(with(&[("auto_confirm", "true"), ("log_retention_days", "7")]).validate().is_ok());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(with(&[("update_channel", "dev")]).validate().is_err());
        assert!(with(&[("auto_confirm", "yes")]).validate().is_err());
        assert!(with(&[("log_retention_days", "-1")]).validate().is_err());
        assert!(with(&[("max_logs_per_day", "5\nauto_confirm=true")]).validate().is_err());
        let mut s = Settings::default();
        s.theme = "blue".into();
        assert!(s.validate().is_err());
    }

    #[test]
    fn conf_holds_only_changed_keys() {
        assert_eq!(Settings::default().linux_util_conf(), None);
        let text = with(&[("verbose", "true")]).linux_util_conf().unwrap();
        assert!(text.ends_with("\nverbose=true\n"), "{text}");
    }
}
