//! Desktop icon, app menu entry and taskbar icon. The packages put the app in
//! the system menu but cannot reach a user's desktop or taskbar, and an
//! AppImage adds itself nowhere, so on its first launch the app offers to add
//! what is missing, and Settings turns each on or off later. The desktop icon
//! and menu entry are launcher files pointing at the one installed program;
//! nothing else is copied. The taskbar icon is a pin of the menu entry in KDE's
//! Task Manager or GNOME's dash, the only taskbars handled. The answer is
//! recorded in `shortcuts.json` in the app config directory so it is asked once.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::settings::write_atomic;

/// The packages install their launcher under this name, and KDE matches it to
/// the window's application ID (see `run` in lib.rs), so ours use it too.
const LAUNCHER: &str = "Linux_Util Desktop.desktop";
/// The packages' launcher, used as it is for a package install and with `Exec`
/// and `Icon` replaced for an AppImage.
const TEMPLATE: &str = include_str!("../../packaging/linux-util-desktop.desktop");
const ICON: &[u8] = include_bytes!("../icons/128x128.png");
const ICON_NAME: &str = "linux_util_desktop.png";

enum Install {
    /// Installed from the `.deb` or `.rpm`, which adds the menu entry itself.
    Package,
    AppImage(PathBuf),
}

/// How the app was installed, or `None` for a source checkout, which gets no
/// shortcuts. update.rs's `install_method` also needs the package manager, so
/// it keeps its own check.
fn install() -> Option<Install> {
    if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file()) {
        return Some(Install::AppImage(path));
    }
    let exe = std::env::current_exe().ok()?;
    exe.starts_with("/usr/").then_some(Install::Package)
}

#[derive(Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    asked: bool,
}

fn record_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("shortcuts.json"))
}

fn asked(app: &AppHandle) -> bool {
    record_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Record>(&t).ok())
        .is_some_and(|r| r.asked)
}

fn desktop_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().desktop_dir().ok().filter(|d| d.is_dir())
}

/// The user's own menu folder (`~/.local/share/applications`).
fn menu_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().data_dir().ok().map(|d| d.join("applications"))
}

/// Whether `file` is a launcher for this app: our file name, or an `Exec` line
/// naming the program or its AppImage (all contain `linux_util_desktop`).
fn launches_app(file: &Path) -> bool {
    if file.file_name().is_some_and(|n| n == LAUNCHER) {
        return true;
    }
    std::fs::read_to_string(file).is_ok_and(|text| {
        text.lines().any(|l| l.strip_prefix("Exec=").is_some_and(|v| v.contains("linux_util_desktop")))
    })
}

/// The launchers for this app in `dir`. A link counts, as "Add to Desktop" in
/// KDE's menu makes one.
fn launchers(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
        .filter(|p| p.is_file() || p.symlink_metadata().is_ok_and(|m| m.is_symlink()))
        .filter(|p| launches_app(p))
        .collect()
}

/// Whether a system menu lists the app: a package installed alongside the
/// AppImage. That entry is the package's, so the app neither adds nor removes one.
fn in_system_menu() -> bool {
    crate::host_var("XDG_DATA_DIRS")
        .map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")])
        .into_iter()
        .any(|d| d.join("applications").join(LAUNCHER).exists())
}

/// The user's menu entry for the app.
fn menu_launcher(app: &AppHandle) -> Option<PathBuf> {
    menu_dir(app).map(|d| d.join(LAUNCHER)).filter(|p| p.symlink_metadata().is_ok())
}

fn icon_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .data_dir()
        .map_err(|e| format!("No data directory: {e}"))?
        .join("icons/hicolor/128x128/apps")
        .join(ICON_NAME))
}

#[derive(Serialize)]
pub struct Status {
    /// A desktop icon can be added: a release install with a desktop folder.
    can_desktop: bool,
    /// A menu entry can be added: an AppImage with no package installed
    /// alongside (the packages add their own).
    can_menu: bool,
    /// A taskbar icon can be pinned: KDE or GNOME, with the menu entry in place.
    can_taskbar: bool,
    /// The AppImage's taskbar icon pins its menu entry, so needs that on.
    taskbar_needs_menu: bool,
    has_desktop: bool,
    has_menu: bool,
    has_taskbar: bool,
    /// The first-launch question has been answered.
    asked: bool,
}

#[tauri::command]
pub fn shortcut_status(app: AppHandle) -> Status {
    let install = install();
    let desktop = desktop_dir(&app);
    let can_menu = matches!(install, Some(Install::AppImage(_))) && !in_system_menu();
    let taskbar = install.as_ref().and_then(|_| taskbar_pinned());
    Status {
        can_desktop: install.is_some() && desktop.is_some(),
        can_menu,
        can_taskbar: taskbar.is_some(),
        taskbar_needs_menu: can_menu,
        has_desktop: desktop.is_some_and(|d| !launchers(&d).is_empty()),
        has_menu: menu_launcher(&app).is_some(),
        has_taskbar: taskbar == Some(true),
        asked: asked(&app),
    }
}

/// Records that the first-launch question was answered.
#[tauri::command]
pub fn shortcuts_asked(app: AppHandle) -> Result<(), String> {
    let path = record_path(&app).ok_or("No app config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(&Record { asked: true }).map_err(|e| e.to_string())?;
    write_atomic(&path, &json)
}

/// `path` as one `Exec` argument: double-quoted with `"`, `` ` ``, `$` and `\`
/// escaped, then the backslashes escaped again and `%` doubled, as the
/// desktop entry spec requires. `None` for a path a launcher cannot hold.
fn exec_arg(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// `path` as a plain string value in a launcher.
fn string_value(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    (!text.chars().any(char::is_control)).then(|| text.replace('\\', "\\\\"))
}

/// The packages' launcher with `Exec` and `Icon` pointed at the AppImage and
/// its icon.
fn appimage_launcher(appimage: &Path, icon: &Path) -> Option<String> {
    let exec = exec_arg(appimage)?;
    let icon = string_value(icon)?;
    let mut text = String::new();
    for line in TEMPLATE.lines() {
        if line.starts_with("Exec=") {
            text.push_str(&format!("Exec={exec}\n"));
        } else if line.starts_with("Icon=") {
            text.push_str(&format!("Icon={icon}\n"));
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    Some(text)
}

/// The launcher text for this install. An AppImage's icon is inside it and
/// unreadable while the app is closed, so it is written out first.
fn launcher_text(app: &AppHandle) -> Result<String, String> {
    match install().ok_or("This copy was not installed from a release file.")? {
        Install::Package => Ok(TEMPLATE.to_string()),
        Install::AppImage(appimage) => {
            let icon = icon_path(app)?;
            if let Some(dir) = icon.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
            }
            std::fs::write(&icon, ICON).map_err(|e| format!("Could not write {}: {e}", icon.display()))?;
            appimage_launcher(&appimage, &icon)
                .ok_or_else(|| format!("{} cannot be used in a launcher.", appimage.display()))
        }
    }
}

/// Writes `text` as a launcher in `dir`. An existing file is left alone.
/// `executable` marks it trusted where desktops ask for that: KDE wants the
/// execute bit on a desktop launcher, GNOME's desktop icons `metadata::trusted`.
fn write_launcher(dir: &Path, text: &str, executable: bool) -> Result<(), String> {
    let path = dir.join(LAUNCHER);
    if path.symlink_metadata().is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    write_atomic(&path, text)?;
    if executable {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Could not make {} executable: {e}", path.display()))?;
        if let Some(gio) = crate::actions::find_in_path("gio") {
            let mut cmd = Command::new(gio);
            cmd.args(["set", "-t", "string"]).arg(&path).args(["metadata::trusted", "true"]);
            crate::use_host_env(&mut cmd);
            let _ = cmd.output();
        }
    }
    Ok(())
}

// ---- taskbar ----

enum Panel {
    Kde,
    Gnome,
}

/// The taskbar this desktop has, from `XDG_CURRENT_DESKTOP` (such as `KDE` or
/// `ubuntu:GNOME`); `None` for any other desktop.
fn panel() -> Option<Panel> {
    let value = crate::host_var("XDG_CURRENT_DESKTOP")?;
    let names = value.to_string_lossy().to_ascii_uppercase();
    let names: Vec<&str> = names.split(':').collect();
    if names.contains(&"KDE") {
        Some(Panel::Kde)
    } else if names.contains(&"GNOME") {
        Some(Panel::Gnome)
    } else {
        None
    }
}

/// Runs a host program and returns what it printed; an error when it is
/// missing or fails.
fn host_output(program: &str, args: &[&str]) -> Result<String, String> {
    let path = crate::actions::find_in_path(program).ok_or_else(|| format!("{program} was not found"))?;
    let mut cmd = Command::new(path);
    cmd.args(args);
    crate::use_host_env(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Could not start {program}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The string literals in GVariant text, in order: `['a', 'b']` gives `a` and
/// `b`, `('text',)` gives `text`. Enough for what gsettings and gdbus print.
fn gvariant_strings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\'' && c != '"' {
            continue;
        }
        let quote = c;
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some(n) => s.push(n),
                    None => break,
                },
                c if c == quote => break,
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

/// `text` as a GVariant string literal.
fn gvariant_string(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A Plasma desktop script that reads, pins or unpins the app in every Task
/// Manager widget (`action` is `status`, `pin` or `unpin`) and prints `none`
/// when there is no Task Manager, otherwise whether the app was pinned before.
/// A pin goes in the first Task Manager; unpinning clears them all.
fn kde_script(action: &str) -> String {
    format!(
        concat!(
            "var action = \"{action}\"; var id = \"applications:{id}\"; ",
            "var mine = function (l) {{ var d = l; try {{ d = decodeURIComponent(l); }} catch (e) {{}} ",
            "return d == id || d.slice(-{len}) == \"/{id}\"; }}; ",
            "var tms = []; panels().forEach(function (p) {{ p.widgets().forEach(function (w) {{ ",
            "if (w.type == \"org.kde.plasma.taskmanager\" || w.type == \"org.kde.plasma.icontasks\") tms.push(w); }}); }}); ",
            "var lists = tms.map(function (w) {{ w.currentConfigGroup = [\"General\"]; ",
            "var l = w.readConfig(\"launchers\", []); if (typeof l == \"string\") l = l ? l.split(\",\") : []; ",
            "return Array.prototype.slice.call(l); }}); ",
            "var pinned = lists.some(function (l) {{ return l.some(mine); }}); ",
            "if (action == \"pin\" && !pinned && tms.length) {{ lists[0].push(id); tms[0].writeConfig(\"launchers\", lists[0]); }} ",
            "if (action == \"unpin\") tms.forEach(function (w, i) {{ var kept = lists[i].filter(function (x) {{ return !mine(x); }}); ",
            "if (kept.length < lists[i].length) w.writeConfig(\"launchers\", kept); }}); ",
            "print(tms.length ? (pinned ? \"pinned\" : \"unpinned\") : \"none\");"
        ),
        action = action,
        id = LAUNCHER,
        len = LAUNCHER.len() + 1,
    )
}

/// Runs `kde_script` in plasmashell; `None` when there is no Task Manager or
/// plasmashell cannot be reached.
fn kde_taskbar(action: &str) -> Result<Option<bool>, String> {
    let script = gvariant_string(&kde_script(action));
    let out = host_output(
        "gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.kde.plasmashell",
            "--object-path",
            "/PlasmaShell",
            "--method",
            "org.kde.PlasmaShell.evaluateScript",
            &script,
        ],
    )?;
    let printed = gvariant_strings(&out).into_iter().next().unwrap_or_default();
    Ok(match printed.trim() {
        "pinned" => Some(true),
        "unpinned" => Some(false),
        _ => None,
    })
}

const GNOME_SCHEMA: &str = "org.gnome.shell";
const GNOME_KEY: &str = "favorite-apps";

fn gnome_favorites() -> Result<Vec<String>, String> {
    host_output("gsettings", &["get", GNOME_SCHEMA, GNOME_KEY]).map(|out| gvariant_strings(&out))
}

fn set_gnome_favorites(list: &[String]) -> Result<(), String> {
    let value = format!("[{}]", list.iter().map(|s| gvariant_string(s)).collect::<Vec<_>>().join(", "));
    host_output("gsettings", &["set", GNOME_SCHEMA, GNOME_KEY, &value]).map(|_| ())
}

/// Whether the app is pinned to the taskbar; `None` when there is no taskbar
/// the app can pin to.
fn taskbar_pinned() -> Option<bool> {
    match panel()? {
        Panel::Kde => kde_taskbar("status").ok().flatten(),
        Panel::Gnome => gnome_favorites().ok().map(|l| l.iter().any(|x| x == LAUNCHER)),
    }
}

fn set_taskbar(pin: bool) -> Result<(), String> {
    match panel() {
        Some(Panel::Kde) => {
            // A menu entry just written may not be in KDE's app index yet, and
            // the Task Manager shows a pin it cannot find as a blank icon.
            if pin {
                let _ = host_output("kbuildsycoca6", &[]).or_else(|_| host_output("kbuildsycoca5", &[]));
            }
            kde_taskbar(if pin { "pin" } else { "unpin" }).map(|_| ())
        }
        Some(Panel::Gnome) => {
            let mut list = gnome_favorites()?;
            let had = list.iter().any(|x| x == LAUNCHER);
            if pin && !had {
                list.push(LAUNCHER.to_string());
            } else if !pin && had {
                list.retain(|x| x != LAUNCHER);
            } else {
                return Ok(());
            }
            set_gnome_favorites(&list)
        }
        None => Ok(()),
    }
}

/// Adds the desktop icon, the menu entry and/or the taskbar icon. What is
/// already there, or cannot be offered for this install, is skipped.
#[tauri::command]
pub fn add_shortcuts(app: AppHandle, desktop: bool, menu: bool, taskbar: bool) -> Result<(), String> {
    let status = shortcut_status(app.clone());
    let desktop = desktop && status.can_desktop && !status.has_desktop;
    let menu = menu && status.can_menu && !status.has_menu;
    if desktop || menu {
        let text = launcher_text(&app)?;
        if desktop {
            let dir = desktop_dir(&app).ok_or("No desktop folder")?;
            write_launcher(&dir, &text, true)?;
        }
        if menu {
            let dir = menu_dir(&app).ok_or("No data directory")?;
            write_launcher(&dir, &text, false)?;
        }
    }
    let menu_ready = !status.taskbar_needs_menu || menu_launcher(&app).is_some();
    if taskbar && status.can_taskbar && !status.has_taskbar && menu_ready {
        set_taskbar(true)?;
    }
    Ok(())
}

fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("Could not remove {}: {e}", path.display())),
        _ => Ok(()),
    }
}

/// Removes the desktop icon, the menu entry and/or the taskbar icon. On the
/// desktop that is every launcher for this app, links included (only the link
/// goes, not what it points at). Removing the AppImage's menu entry unpins it
/// too, as the pin would point at nothing. The AppImage's icon copy goes once
/// neither launcher is left.
#[tauri::command]
pub fn remove_shortcuts(app: AppHandle, desktop: bool, menu: bool, taskbar: bool) -> Result<(), String> {
    let status = shortcut_status(app.clone());
    if status.has_taskbar && (taskbar || (menu && status.taskbar_needs_menu)) {
        set_taskbar(false)?;
    }
    if desktop {
        if let Some(dir) = desktop_dir(&app) {
            for path in launchers(&dir) {
                remove(&path)?;
            }
        }
    }
    if menu {
        if let Some(path) = menu_launcher(&app) {
            remove(&path)?;
        }
    }
    let status = shortcut_status(app.clone());
    if !status.has_desktop && !status.has_menu {
        remove(&icon_path(&app)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_arg_quotes_and_escapes() {
        assert_eq!(exec_arg(Path::new("/home/a/Apps/x.AppImage")).unwrap(), "\"/home/a/Apps/x.AppImage\"");
        assert_eq!(exec_arg(Path::new("/home/a/My Apps/x.AppImage")).unwrap(), "\"/home/a/My Apps/x.AppImage\"");
        assert_eq!(exec_arg(Path::new("/a/$b\"c`d")).unwrap(), r#""/a/\\$b\\"c\\`d""#);
        assert_eq!(exec_arg(Path::new(r"/a/b\c")).unwrap(), r#""/a/b\\\\c""#);
        assert_eq!(exec_arg(Path::new("/a/100%")).unwrap(), "\"/a/100%%\"");
        assert!(exec_arg(Path::new("/a/b\nExec=evil")).is_none());
    }

    #[test]
    fn appimage_launcher_replaces_exec_and_icon_only() {
        let text = appimage_launcher(Path::new("/opt/x.AppImage"), Path::new("/h/icon.png")).unwrap();
        assert!(text.contains("\nExec=\"/opt/x.AppImage\"\n"), "{text}");
        assert!(text.contains("\nIcon=/h/icon.png\n"), "{text}");
        assert!(!text.contains("Exec=linux_util_desktop"), "{text}");
        assert!(text.starts_with("[Desktop Entry]\n"), "{text}");
        assert!(text.contains("\nName=Linux_Util Desktop\n"), "{text}");
        assert!(appimage_launcher(Path::new("/opt/x.AppImage"), Path::new("/h/i\n.png")).is_none());
    }

    #[test]
    fn reads_and_writes_gvariant_strings() {
        assert_eq!(gvariant_strings("['a.desktop', 'Linux_Util Desktop.desktop']\n"), ["a.desktop", "Linux_Util Desktop.desktop"]);
        assert!(gvariant_strings("@as []\n").is_empty());
        assert_eq!(gvariant_strings("('pinned\\n',)\n"), ["pinned\n"]);
        assert_eq!(gvariant_strings(r#"["it's", 'a\'b\\c']"#), ["it's", "a'b\\c"]);
        assert_eq!(gvariant_string(r"a'b\c"), r"'a\'b\\c'");
        let list = vec!["x.desktop".to_string(), "it's".to_string()];
        let text = format!("[{}]", list.iter().map(|s| gvariant_string(s)).collect::<Vec<_>>().join(", "));
        assert_eq!(gvariant_strings(&text), list);
    }

    #[test]
    fn kde_script_is_one_line_for_the_launcher() {
        let script = kde_script("pin");
        assert!(!script.contains('\n'));
        assert!(script.contains(r#"var id = "applications:Linux_Util Desktop.desktop";"#), "{script}");
        assert!(script.contains(r#"d.slice(-27) == "/Linux_Util Desktop.desktop""#), "{script}");
    }

    #[test]
    fn finds_existing_launchers() {
        let dir = std::env::temp_dir().join(format!("lud-shortcuts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("other.desktop"), "[Desktop Entry]\nExec=firefox\n").unwrap();
        assert!(launchers(&dir).is_empty());
        std::fs::write(dir.join("mine.desktop"), "[Desktop Entry]\nExec=\"/a/linux_util_desktop_0.2.2_amd64.AppImage\"\n").unwrap();
        assert_eq!(launchers(&dir), vec![dir.join("mine.desktop")]);
        std::fs::remove_file(dir.join("mine.desktop")).unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", dir.join(LAUNCHER)).unwrap();
        assert_eq!(launchers(&dir), vec![dir.join(LAUNCHER)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
