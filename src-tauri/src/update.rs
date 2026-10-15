//! Version information and updating the app from its GitHub releases.
//!
//! The check reads the latest stable release of this repo, or, when the
//! "beta updates" setting is on, the newest release including beta
//! pre-releases. Installing downloads the
//! file that matches how the app was installed, checks it against the
//! release's `SHA256SUMS`, and then either hands a `.deb` or `.rpm` to the
//! system package manager through the same password dialog the install actions
//! use, or replaces the AppImage file in place. Nothing is downloaded without a
//! click, and no checksum means no install.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State};

use crate::actions::{self, RunState};

const REPO: &str = "acebmxer/linux_util_desktop";
/// Replaces the GitHub API address for the release check and lifts the
/// download address restriction. Only for testing against a local server.
const RELEASES_URL_ENV: &str = "LU_RELEASES_URL";
const SUMS_NAME: &str = "SHA256SUMS";
/// Largest download accepted: the AppImage is well under this.
const MAX_DOWNLOAD: u64 = 1 << 30;
const MAX_NOTES: usize = 20_000;
/// Set by the dev-branch release build to its beta number; a stable
/// build has none. It tells the update check which beta this copy is.
const BETA_BUILD: Option<&str> = option_env!("LU_BETA_BUILD");
/// Sorts a stable release above every beta of the same version.
const STABLE: u64 = u64::MAX;

/// A release's place in time: version numbers, then beta number (`STABLE` for
/// a stable release).
type Rank = ((u64, u64, u64), u64);

#[derive(Serialize)]
pub struct VersionInfo {
    app: String,
    /// The bundled linux_util release (`vendor/linux_util.VERSION`).
    linux_util: Option<String>,
}

#[derive(Serialize)]
pub struct Update {
    version: String,
    /// The release notes as written on GitHub (plain text, not rendered).
    notes: String,
    page: String,
    can_install: bool,
    /// How the update would be installed, or why it cannot be from here.
    note: String,
}

struct Release {
    version: String,
    /// The tag without its `v`, such as `0.2.1` or `0.2.1-beta.12`.
    label: String,
    rank: Rank,
    notes: String,
    page: String,
    /// (file name, download address)
    assets: Vec<(String, String)>,
}

enum Method {
    AppImage(PathBuf),
    Deb,
    /// The package manager's program: `dnf` or `zypper`.
    Rpm(&'static str),
}

struct Plan {
    method: Method,
    file: (String, String),
    sums: String,
}

impl Plan {
    fn describe(&self) -> String {
        match &self.method {
            Method::AppImage(path) => format!("Replaces the AppImage at {}.", path.display()),
            Method::Deb => "Installs the .deb with apt-get; asks for your password.".to_string(),
            Method::Rpm(pm) => format!("Installs the .rpm with {pm}; asks for your password."),
        }
    }
}

/// `1.2.3` or `v1.2.3` as numbers. A pre-release or build suffix is ignored.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// A release tag as a `Rank`: `v0.2.1` is stable, `v0.2.1-beta.12` is beta 12,
/// any other pre-release suffix sorts below every beta.
fn parse_release(tag: &str) -> Option<Rank> {
    let version = parse_version(tag)?;
    let tag = tag.trim().trim_start_matches('v');
    let beta = match tag.split_once('-') {
        None => STABLE,
        Some((_, pre)) => pre
            .strip_prefix("beta.")
            .and_then(|n| n.split('+').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
    };
    Some((version, beta))
}

/// The running app's `Rank` from its version and its beta build number, if any.
fn running_rank(version: &str, beta: Option<&str>) -> Option<Rank> {
    let version = parse_version(version)?;
    Some((version, beta.and_then(|n| n.parse().ok()).unwrap_or(STABLE)))
}

/// The running app's version as shown: `0.2.1`, or `0.2.1-beta.12` for a beta build.
fn running_label(version: &str, beta: Option<&str>) -> String {
    match beta.and_then(|n| n.parse::<u64>().ok()) {
        Some(n) => format!("{version}-beta.{n}"),
        None => version.to_string(),
    }
}

fn agent(download: bool) -> ureq::Agent {
    let builder = ureq::Agent::config_builder().user_agent(format!("linux-util-desktop/{}", env!("CARGO_PKG_VERSION")));
    let builder = if download {
        // A big file on a slow line is fine; a stalled connection is not.
        builder.timeout_connect(Some(Duration::from_secs(15))).timeout_recv_response(Some(Duration::from_secs(30)))
    } else {
        builder.timeout_global(Some(Duration::from_secs(20)))
    };
    builder.build().into()
}

fn releases_url(beta: bool) -> (String, bool) {
    match std::env::var(RELEASES_URL_ENV) {
        Ok(url) if !url.is_empty() => (url, true),
        // `latest` never returns a pre-release; the list does.
        _ if beta => (format!("https://api.github.com/repos/{REPO}/releases?per_page=30"), false),
        _ => (format!("https://api.github.com/repos/{REPO}/releases/latest"), false),
    }
}

/// Files may only come from this repo's own releases (unless testing).
fn download_allowed(url: &str, overridden: bool) -> bool {
    overridden || url.starts_with(&format!("https://github.com/{REPO}/releases/download/"))
}

fn release_from(doc: &serde_json::Value, overridden: bool) -> Result<Release, String> {
    let tag = doc["tag_name"].as_str().ok_or("The release has no tag")?;
    let rank = parse_release(tag).ok_or_else(|| format!("Cannot read the release version {tag}"))?;
    let ((major, minor, patch), _) = rank;
    let assets = doc["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str()?.to_string())))
                .filter(|(_, url)| download_allowed(url, overridden))
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        version: format!("{major}.{minor}.{patch}"),
        label: tag.trim().trim_start_matches('v').to_string(),
        rank,
        notes: doc["body"].as_str().unwrap_or("").chars().take(MAX_NOTES).collect(),
        page: doc["html_url"].as_str().unwrap_or("").to_string(),
        assets,
    })
}

/// The newest release of the chosen channel: the latest stable one, or the
/// newest of all (betas included) when `beta` is set.
fn fetch_release(beta: bool) -> Result<Release, String> {
    let (url, overridden) = releases_url(beta);
    let mut response = agent(false)
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("Could not check for updates: {e}"))?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Could not read the release information: {e}"))?;
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Unexpected release information: {e}"))?;
    match doc.as_array() {
        None => release_from(&doc, overridden),
        Some(list) => list
            .iter()
            .filter(|d| d["draft"].as_bool() != Some(true))
            .filter_map(|d| release_from(d, overridden).ok())
            .max_by_key(|r| r.rank)
            .ok_or_else(|| "No releases were found".to_string()),
    }
}

fn install_method() -> Result<Method, String> {
    if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()) {
        return Ok(Method::AppImage(path));
    }
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find the running app: {e}"))?;
    if !exe.starts_with("/usr/") {
        return Err("This copy was not installed from a release file, so it cannot update itself.".to_string());
    }
    if actions::find_in_path("apt-get").is_some() {
        Ok(Method::Deb)
    } else if actions::find_in_path("dnf").is_some() {
        Ok(Method::Rpm("dnf"))
    } else if actions::find_in_path("zypper").is_some() {
        Ok(Method::Rpm("zypper"))
    } else {
        Err("No supported package manager (apt-get, dnf or zypper) was found.".to_string())
    }
}

fn plan(release: &Release) -> Result<Plan, String> {
    if std::env::consts::ARCH != "x86_64" {
        return Err("Releases are built for x86_64 only.".to_string());
    }
    let method = install_method()?;
    let v = &release.version;
    let name = match &method {
        Method::AppImage(_) => format!("linux_util_desktop_{v}_amd64.AppImage"),
        Method::Deb => format!("linux_util_desktop_{v}_amd64.deb"),
        Method::Rpm(_) => format!("linux_util_desktop-{v}-1.x86_64.rpm"),
    };
    let find = |wanted: &str| release.assets.iter().find(|(n, _)| n == wanted).cloned();
    let file = find(&name).ok_or_else(|| format!("Release {v} has no {name}."))?;
    let sums = find(SUMS_NAME).ok_or_else(|| format!("Release {v} has no {SUMS_NAME}, so the download cannot be checked."))?;
    Ok(Plan { method, file, sums: sums.1 })
}

/// The hash `sums` (the `sha256sum` format) lists for `name`.
fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads `url` to `dest`, reporting progress in tenths when the size is known.
/// `dest` must not exist yet: it is created exclusively, so a file or link put
/// there beforehand is never written through. Stops when the run is cancelled.
fn download(app: &AppHandle, url: &str, dest: &Path) -> Result<(), String> {
    let mut response = agent(true).get(url).call().map_err(|e| format!("Download failed: {e}"))?;
    let total = response.body().content_length();
    let mut reader = response.body_mut().with_config().limit(MAX_DOWNLOAD).reader();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dest)
        .map_err(|e| format!("Could not write {}: {e}", dest.display()))?;
    let (mut done, mut reported) = (0u64, 0u64);
    let mut buf = [0u8; 64 * 1024];
    loop {
        if actions::is_cancelled(app) {
            return Err("Cancelled".to_string());
        }
        let n = reader.read(&mut buf).map_err(|e| format!("Download failed: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("Could not write {}: {e}", dest.display()))?;
        done += n as u64;
        if let Some(total) = total.filter(|t| *t > 0) {
            let tenth = done * 10 / total;
            if tenth > reported {
                reported = tenth;
                actions::emit_output(app, &format!("  {}%", tenth * 10), false);
            }
        }
    }
    file.flush().map_err(|e| format!("Could not write {}: {e}", dest.display()))
}

/// Downloads the plan's file next to `dest_dir`'s `file_name` and checks it
/// against the release's checksums. Returns the verified file's path.
fn fetch_verified(app: &AppHandle, plan: &Plan, dest_dir: &Path, file_name: &str) -> Result<PathBuf, String> {
    let (name, url) = &plan.file;
    actions::emit_output(app, &format!("Downloading {SUMS_NAME}"), false);
    let sums = agent(false)
        .get(&plan.sums)
        .call()
        .and_then(|mut r| r.body_mut().read_to_string())
        .map_err(|e| format!("Could not download {SUMS_NAME}: {e}"))?;
    let expected = expected_hash(&sums, name).ok_or_else(|| format!("{SUMS_NAME} does not list {name}."))?;
    actions::emit_output(app, &format!("Downloading {name}"), false);
    let dest = dest_dir.join(file_name);
    if let Err(e) = download(app, url, &dest) {
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }
    actions::emit_output(app, "Checking the download against the checksum", false);
    let actual = sha256_of(&dest).map_err(|e| format!("Could not read {}: {e}", dest.display()))?;
    if actual != expected {
        let _ = std::fs::remove_file(&dest);
        return Err(format!("The download does not match its checksum (expected {expected}, got {actual}). Nothing was installed."));
    }
    actions::emit_output(app, &format!("Checksum matches ({expected})"), false);
    Ok(dest)
}

/// A private (mode 0700) folder for the download, removed afterwards. The file
/// is installed as root, so nobody else may be able to swap it after the check.
struct Scratch(PathBuf);

impl Scratch {
    fn create(app: &AppHandle) -> Result<Self, String> {
        let base = app.path().app_cache_dir().map_err(|e| format!("No cache directory: {e}"))?;
        std::fs::create_dir_all(&base).map_err(|e| format!("Could not create {}: {e}", base.display()))?;
        let dir = base.join(format!("update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        Ok(Scratch(dir))
    }

    /// A new private folder inside `base`. Created exclusively under a fresh
    /// name, so nothing another user put in `base` can be reused or followed.
    fn new_in(base: &Path) -> Result<Self, String> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let mut last = None;
        for n in 0..16 {
            let dir = base.join(format!(".linux-util-desktop-update-{}-{stamp}-{n}", std::process::id()));
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Ok(Scratch(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(format!("Could not create {}: {e}", dir.display())),
            }
        }
        Err(format!("Could not create a folder in {}: {}", base.display(), last.map(|e| e.to_string()).unwrap_or_default()))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Puts the verified AppImage over the running one. The download goes to a
/// private folder next to it (same filesystem, so the rename is atomic) and the
/// running copy keeps working until the app is restarted.
fn replace_appimage(plan: &Plan, target: &Path, app: &AppHandle) -> Result<(), String> {
    let dir = target.parent().ok_or("The AppImage has no folder")?;
    let scratch = Scratch::new_in(dir)?;
    let staged = fetch_verified(app, plan, &scratch.0, &plan.file.0)?;
    if actions::is_cancelled(app) {
        return Err("Cancelled".to_string());
    }
    std::fs::metadata(target)
        .map(|m| m.permissions().mode() & 0o777 | 0o755)
        .and_then(|mode| std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(mode)))
        .and_then(|_| std::fs::rename(&staged, target))
        .map_err(|e| format!("Could not replace {}: {e}", target.display()))
}

/// `reinstall` is for a file with the same package version as the installed
/// one (a newer beta of the same version): a plain install would do nothing.
fn package_command(method: &Method, file: &Path, reinstall: bool) -> Command {
    let mut cmd = Command::new("sudo");
    match method {
        Method::Deb => {
            cmd.env("DEBIAN_FRONTEND", "noninteractive").args(["apt-get", "install", "-y"]);
            if reinstall {
                cmd.arg("--reinstall");
            }
        }
        Method::Rpm("zypper") => {
            cmd.args(["zypper", "--non-interactive", "install", "--allow-unsigned-rpm"]);
            if reinstall {
                cmd.arg("--force");
            }
        }
        _ => {
            cmd.args(["dnf", if reinstall { "reinstall" } else { "install" }, "-y"]);
        }
    }
    cmd.arg(file);
    crate::use_host_env(&mut cmd);
    cmd
}

fn run_update(app: &AppHandle, version: &str) {
    let label = format!("Linux_Util Desktop {version}");
    let beta = crate::settings::beta_updates(app);
    let installed = app.package_info().version.to_string();
    let mut reinstall = false;
    let names = [label.clone()];
    // A step that fails because Cancel was pressed ends the run as cancelled.
    let fail = |message: String| {
        if actions::is_cancelled(app) {
            actions::emit_item(app, 0, &label, "cancelled", None);
            return actions::emit_done(app, 0, 0, true);
        }
        actions::emit_output(app, &message, true);
        actions::emit_item(app, 0, &label, "failed", None);
        actions::emit_done(app, 0, 1, false);
    };
    actions::emit_item(app, 0, &label, "running", None);
    actions::emit_output(app, "Checking the latest release", false);
    let plan = match fetch_release(beta).and_then(|release| {
        if release.label != version {
            return Err(format!("Version {} is now the latest, not {version}. Check for updates again.", release.label));
        }
        reinstall = release.version == installed;
        plan(&release)
    }) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    if actions::is_cancelled(app) {
        return fail("Cancelled".to_string());
    }
    match &plan.method {
        Method::AppImage(target) => match replace_appimage(&plan, target, app) {
            Ok(()) => {
                actions::emit_output(app, &format!("Replaced {}", target.display()), false);
                actions::emit_item(app, 0, &label, "ok", None);
                actions::emit_done(app, 1, 0, false);
            }
            Err(e) => fail(e),
        },
        method => {
            let scratch = match Scratch::create(app) {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let file = match fetch_verified(app, &plan, &scratch.0, &plan.file.0) {
                Ok(f) => f,
                Err(e) => return fail(e),
            };
            if actions::is_cancelled(app) {
                return fail("Cancelled".to_string());
            }
            actions::run_queue(app, &names, &|_| package_command(method, &file, reinstall));
        }
    }
}

/// The running version and the bundled linux_util version.
#[tauri::command]
pub fn version_info(app: AppHandle) -> VersionInfo {
    let linux_util = crate::script_path(&app)
        .ok()
        .and_then(|script| std::fs::read_to_string(script.with_file_name("linux_util.VERSION")).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    VersionInfo { app: running_label(&app.package_info().version.to_string(), BETA_BUILD), linux_util }
}

/// The newest release of the chosen channel when it is newer than the running
/// app, else `None`.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<Update>, String> {
    let beta = crate::settings::beta_updates(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let release = fetch_release(beta)?;
        let current = running_rank(&app.package_info().version.to_string(), BETA_BUILD).ok_or("Cannot read the app version")?;
        if release.rank <= current {
            return Ok(None);
        }
        let (can_install, note) = match plan(&release) {
            Ok(p) => (true, p.describe()),
            Err(e) => (false, e),
        };
        Ok(Some(Update { version: release.label, notes: release.notes, page: release.page, can_install, note }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Installs release `version`, as a run in the action panel. The run starts
/// at once, so Cancel works from the first moment; it fails when the newest
/// release is no longer the one the user was shown.
#[tauri::command]
pub fn install_update(app: AppHandle, state: State<'_, RunState>, version: String) -> Result<(), String> {
    actions::spawn_run(&app, &state, move |app| run_update(app, &version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(parse_version("v0.2.10"), Some((0, 2, 10)));
        assert_eq!(parse_version("1.0.0-rc1"), Some((1, 0, 0)));
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("1.0"), None);
        assert_eq!(parse_version("1.0.0.1"), None);
        assert_eq!(parse_version("latest"), None);
    }

    #[test]
    fn releases_rank_stable_above_their_betas() {
        let r = |t| parse_release(t).unwrap();
        assert!(r("v0.2.1") > r("v0.2.1-beta.12"));
        assert!(r("v0.2.1-beta.12") > r("v0.2.1-beta.9"));
        assert!(r("v0.2.1-beta.1") > r("v0.2.0"));
        assert!(r("v0.2.1-rc1") < r("v0.2.1-beta.1"));
        assert_eq!(parse_release("latest"), None);
    }

    #[test]
    fn running_app_ranks_like_its_release() {
        let stable = running_rank("0.2.1", None);
        let beta = running_rank("0.2.1", Some("12"));
        assert_eq!(stable, parse_release("v0.2.1"));
        assert_eq!(beta, parse_release("v0.2.1-beta.12"));
        // An empty build variable (a stable build) is not a beta.
        assert_eq!(running_rank("0.2.1", Some("")), stable);
        assert_eq!(running_label("0.2.1", Some("12")), "0.2.1-beta.12");
        assert_eq!(running_label("0.2.1", Some("")), "0.2.1");
    }

    #[test]
    fn newest_release_wins_and_drafts_are_skipped() {
        let doc = |tag: &str, draft: bool| serde_json::json!({"tag_name": tag, "draft": draft});
        let list = serde_json::json!([doc("v0.2.1-beta.3", false), doc("v0.2.1", true), doc("v0.2.1-beta.2", false), doc("v0.2.0", false)]);
        let best = list
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["draft"].as_bool() != Some(true))
            .filter_map(|d| release_from(d, false).ok())
            .max_by_key(|r| r.rank)
            .unwrap();
        assert_eq!(best.label, "0.2.1-beta.3");
        assert_eq!(best.version, "0.2.1");
    }

    #[test]
    fn checksum_lines() {
        let hash = "a".repeat(64);
        let sums = format!("{hash}  one.deb\n{}  *two.rpm\nshort  three\n", "B".repeat(64));
        assert_eq!(expected_hash(&sums, "one.deb"), Some(hash));
        assert_eq!(expected_hash(&sums, "two.rpm"), Some("b".repeat(64)));
        assert_eq!(expected_hash(&sums, "three"), None);
        assert_eq!(expected_hash(&sums, "missing"), None);
        assert_eq!(expected_hash(&sums, "one"), None);
    }

    #[test]
    fn only_this_repos_releases_download() {
        let ok = format!("https://github.com/{REPO}/releases/download/v1.0.0/x.deb");
        assert!(download_allowed(&ok, false));
        assert!(!download_allowed("https://github.com/other/repo/releases/download/v1/x.deb", false));
        assert!(!download_allowed("http://example.com/x.deb", false));
        assert!(download_allowed("http://127.0.0.1:1/x.deb", true));
    }

    #[test]
    fn hashes_a_file() {
        let path = std::env::temp_dir().join(format!("lud-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let hash = sha256_of(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(hash, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
