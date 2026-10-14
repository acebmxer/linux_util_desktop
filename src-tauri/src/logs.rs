//! Read-only access to linux_util's own logs: the `logs/` directory beside the
//! per-user copy that actions run from (see `actions::staged_dir`).

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use serde::Serialize;
use tauri::AppHandle;

/// Longer logs are shown from their end, where the latest output is.
const MAX_SHOWN_BYTES: u64 = 512 * 1024;

#[derive(Serialize)]
pub struct LogFile {
    name: String,
    size: u64,
    /// Seconds since the Unix epoch.
    modified: u64,
}

#[derive(Serialize)]
pub struct LogText {
    path: String,
    text: String,
    truncated: bool,
}

fn log_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::actions::staged_dir(app)?.join("logs"))
}

/// The log files, newest first. `*_latest.log` links are left out: each points
/// at a file that is already listed.
#[tauri::command]
pub fn list_logs(app: AppHandle) -> Result<Vec<LogFile>, String> {
    let dir = log_dir(&app)?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Could not read {}: {e}", dir.display())),
    };
    let mut files: Vec<LogFile> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let meta = entry.metadata().ok()?; // does not follow symlinks
            if !name.ends_with(".log") || !meta.file_type().is_file() {
                return None;
            }
            let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some(LogFile { name, size: meta.len(), modified })
        })
        .collect();
    files.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| b.name.cmp(&a.name)));
    Ok(files)
}

#[tauri::command]
pub fn read_log(app: AppHandle, name: String) -> Result<LogText, String> {
    // Only plain names of files listed above: no paths, no links.
    if name.contains('/') || name.starts_with('.') || !name.ends_with(".log") {
        return Err(format!("Not a log file: {name}"));
    }
    let path = log_dir(&app)?.join(&name);
    let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("Could not read {name}: {e}"))?;
    if !meta.file_type().is_file() {
        return Err(format!("Not a log file: {name}"));
    }
    let mut file = std::fs::File::open(&path).map_err(|e| format!("Could not read {name}: {e}"))?;
    let truncated = meta.len() > MAX_SHOWN_BYTES;
    if truncated {
        file.seek(SeekFrom::End(-(MAX_SHOWN_BYTES as i64))).map_err(|e| e.to_string())?;
    }
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(|e| format!("Could not read {name}: {e}"))?;
    let mut lines: Vec<String> = raw.split(|b| *b == b'\n').map(crate::actions::clean_line).collect();
    if truncated && !lines.is_empty() {
        lines.remove(0); // the cut most likely fell inside this line
    }
    Ok(LogText { path: path.display().to_string(), text: lines.join("\n"), truncated })
}
