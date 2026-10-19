//! Remembers the window size and the sidebar width between runs, in
//! `window.json` in the app config directory. Kept apart from `settings.json`
//! so the Settings dialog never overwrites it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, LogicalSize, Manager, WebviewWindow};

const MIN_WIDTH: f64 = 640.0;
const MIN_HEIGHT: f64 = 420.0;
const MAX_SIDEBAR: u32 = 600;

#[derive(Default, Serialize, Deserialize)]
struct State {
    width: Option<f64>,
    height: Option<f64>,
    #[serde(default)]
    maximized: bool,
    sidebar: Option<u32>,
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("window.json"))
}

fn load(app: &AppHandle) -> State {
    path(app).and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn store(app: &AppHandle, state: &State) -> Result<(), String> {
    let path = path(app).ok_or("No app config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| format!("Could not write {}: {e}", path.display()))
}

/// Applies the remembered size to the main window. Called once at startup.
pub(crate) fn restore(window: &WebviewWindow) {
    let state = load(window.app_handle());
    if let (Some(w), Some(h)) = (state.width, state.height) {
        if w.is_finite() && h.is_finite() {
            let _ = window.set_size(LogicalSize::new(w.clamp(MIN_WIDTH, 10000.0), h.clamp(MIN_HEIGHT, 10000.0)));
        }
    }
    if state.maximized {
        let _ = window.maximize();
    }
}

/// Records the window's size. While maximized or fullscreen the earlier
/// normal size is kept, so un-maximizing after a restart gives it back.
pub(crate) fn save(window: &WebviewWindow) {
    let app = window.app_handle();
    let mut state = load(app);
    state.maximized = window.is_maximized().unwrap_or(false);
    if !state.maximized && !window.is_fullscreen().unwrap_or(false) {
        if let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) {
            let logical = size.to_logical::<f64>(scale);
            state.width = Some(logical.width);
            state.height = Some(logical.height);
        }
    }
    let _ = store(app, &state);
}

#[tauri::command]
pub fn get_sidebar_width(app: AppHandle) -> Option<u32> {
    load(&app).sidebar.filter(|w| *w <= MAX_SIDEBAR)
}

#[tauri::command]
pub fn set_sidebar_width(app: AppHandle, width: u32) -> Result<(), String> {
    if width > MAX_SIDEBAR {
        return Err(format!("Sidebar width out of range: {width}"));
    }
    let mut state = load(&app);
    state.sidebar = Some(width);
    store(&app, &state)
}
