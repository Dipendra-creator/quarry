//! Self-update from the project's GitHub releases.
//!
//! Quarry asks GitHub for the latest release, picks the package that matches
//! how this copy was installed, checks it against the release's
//! `SHA256SUMS.txt`, puts it in place and restarts:
//!
//! - macOS: the zipped `Quarry.app` replaces the running bundle.
//! - Windows, installed: the new installer runs silently over the old one.
//! - Windows portable and Linux tarball: the files next to the executable
//!   are replaced (a running executable can be renamed on both).
//! - Linux DEB: the package is downloaded and handed to the system installer,
//!   because `/usr/bin` needs root.
//!
//! Builds run from a Cargo `target` directory only link to the release page.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui_kit::{App, BorrowAppContext as _, Global, Window};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::prompt;
use crate::state::{AppState, db};

/// Where releases are published.
pub const REPO: &str = "Dipendra-creator/quarry";
/// How often the automatic check runs at most.
const CHECK_EVERY: Duration = Duration::from_secs(12 * 60 * 60);

// ---- versions and releases ---------------------------------------------------

/// `major.minor.patch`; a pre-release suffix (`-beta.1`) is ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let core = s.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>());
        let v = Version(parts.next()?.ok()?, parts.next().unwrap_or(Ok(0)).ok()?, parts.next().unwrap_or(Ok(0)).ok()?);
        parts.next().is_none().then_some(v)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The version of this build. `QUARRY_UPDATE_TEST_VERSION` pretends to be an
/// older build, to try the updater against a published release.
pub fn current_version() -> Version {
    std::env::var("QUARRY_UPDATE_TEST_VERSION")
        .ok()
        .and_then(|v| Version::parse(&v))
        .or_else(|| Version::parse(env!("CARGO_PKG_VERSION")))
        .unwrap_or(Version(0, 0, 0))
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag_name)
    }

    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("Quarry/{} (self-update)", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .build()?)
}

/// The newest published (non-draft, non-prerelease) release.
pub async fn latest_release() -> Result<Release> {
    let resp = http()?
        .get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("could not reach GitHub")?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("no release has been published yet");
    }
    let body = resp.error_for_status().context("GitHub refused the update check")?.bytes().await?;
    Ok(serde_json::from_slice(&body).context("unexpected answer from GitHub")?)
}

// ---- installs ------------------------------------------------------------------

/// How this copy of Quarry was installed.
#[derive(Clone, Debug, PartialEq)]
pub enum Install {
    /// `…/Quarry.app`.
    MacApp(PathBuf),
    /// Installed by the Windows installer (an uninstaller sits next to it).
    WindowsInstaller,
    /// Extracted from a ZIP (Windows) or tarball (Linux); the folder holding the executables.
    Portable(PathBuf),
    /// Installed by the DEB into a folder only root may change.
    LinuxPackage,
    /// Run from a Cargo build directory.
    Development,
}

pub fn detect_install() -> Install {
    let Ok(exe) = std::env::current_exe().and_then(|p| p.canonicalize()) else { return Install::Development };
    detect_install_at(&exe)
}

fn detect_install_at(exe: &Path) -> Install {
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    if dir.components().any(|c| c.as_os_str() == "target") && dir.ancestors().any(|a| a.join("Cargo.toml").exists()) {
        return Install::Development;
    }
    if cfg!(target_os = "macos") {
        // …/Quarry.app/Contents/MacOS/quarry
        return match exe.ancestors().nth(3) {
            Some(bundle) if bundle.extension().is_some_and(|e| e == "app") => Install::MacApp(bundle.to_path_buf()),
            _ => Install::Development,
        };
    }
    if cfg!(windows) {
        return if dir.join("unins000.exe").exists() { Install::WindowsInstaller } else { Install::Portable(dir) };
    }
    if is_writable(&dir) { Install::Portable(dir) } else { Install::LinuxPackage }
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".quarry-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// `x64` or `arm64`, as release packages name the CPU.
fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" }
}

/// The release package for this kind of install.
pub fn asset_name(install: &Install, v: Version) -> Option<String> {
    let arch = arch();
    Some(match install {
        Install::MacApp(_) => format!("Quarry-{v}-macos-{arch}.zip"),
        Install::WindowsInstaller => format!("QuarrySetup-{v}-{arch}.exe"),
        Install::Portable(_) if cfg!(windows) => format!("Quarry-{v}-windows-{arch}.zip"),
        Install::Portable(_) => format!("Quarry-{v}-linux-{arch}.tar.gz"),
        Install::LinuxPackage => format!("Quarry-{v}-linux-{arch}.deb"),
        Install::Development => return None,
    })
}

/// `name → sha256` from a `sha256sum`-style file.
pub fn parse_checksums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim().trim_start_matches('*');
            (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())).then(|| (name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads `asset` into `dir` and checks it against `SHA256SUMS.txt`.
async fn download(release: &Release, asset: &Asset, dir: &Path, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    let client = http()?;
    let sums_asset = release.asset("SHA256SUMS.txt").ok_or_else(|| anyhow!("the release has no SHA256SUMS.txt"))?;
    let sums = client.get(&sums_asset.url).send().await?.error_for_status()?.text().await?;
    let expected = parse_checksums(&sums)
        .remove(&asset.name)
        .ok_or_else(|| anyhow!("SHA256SUMS.txt does not list {}", asset.name))?;

    std::fs::create_dir_all(dir)?;
    let path = dir.join(&asset.name);
    let mut resp = client.get(&asset.url).send().await?.error_for_status().with_context(|| format!("could not download {}", asset.name))?;
    let total = resp.content_length().unwrap_or(asset.size);
    let mut file = std::fs::File::create(&path)?;
    let mut done = 0u64;
    while let Some(chunk) = resp.chunk().await? {
        std::io::Write::write_all(&mut file, &chunk)?;
        done += chunk.len() as u64;
        progress(done, total);
    }
    drop(file);
    let actual = sha256_file(&path)?;
    if actual != expected {
        let _ = std::fs::remove_file(&path);
        bail!("{} failed its checksum (expected {expected}, got {actual}); nothing was installed", asset.name);
    }
    Ok(path)
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.stdin(Stdio::null()).output().with_context(|| format!("could not run {:?}", cmd.get_program()))?;
    if !out.status.success() {
        bail!("{:?} failed: {}", cmd.get_program(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// What to do once the new version is in place.
enum Finish {
    /// Start this program after Quarry exits, then quit.
    Restart(Command),
    /// Nothing to restart (the package manager takes over); show this message.
    Message(String),
}

/// Waits for this process to exit, then runs `program` with `args` (Unix).
fn after_exit(program: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(r#"pid="$1"; shift; while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done; exec "$@""#)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(program)
        .args(args);
    cmd
}

/// Puts the downloaded package in place. Runs off the UI thread.
fn install_package(install: &Install, package: &Path, work: &Path) -> Result<Finish> {
    match install {
        Install::MacApp(bundle) => {
            let parent = bundle.parent().ok_or_else(|| anyhow!("unexpected app location"))?;
            // Unpack next to the app, so the swap is two renames on one volume.
            let stage = parent.join(format!(".quarry-update-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&stage);
            std::fs::create_dir(&stage).with_context(|| {
                format!("Quarry cannot write to {}. Move Quarry.app to a folder you can change, or install the update by hand", parent.display())
            })?;
            let result = (|| {
                run(Command::new("ditto").args(["-x", "-k"]).arg(package).arg(&stage))?;
                let fresh = stage.join("Quarry.app");
                if !fresh.join("Contents/MacOS/quarry").is_file() {
                    bail!("the downloaded package does not contain Quarry.app");
                }
                run(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&fresh))
                    .context("the downloaded app failed its signature check")?;
                let old = stage.join("previous.app");
                std::fs::rename(bundle, &old).context("could not move the current app aside")?;
                if let Err(e) = std::fs::rename(&fresh, bundle) {
                    let _ = std::fs::rename(&old, bundle);
                    return Err(anyhow::Error::from(e).context("could not move the new app into place"));
                }
                Ok(())
            })();
            let _ = std::fs::remove_dir_all(&stage);
            result?;
            let bundle = bundle.to_string_lossy().into_owned();
            Ok(Finish::Restart(after_exit("/usr/bin/open", &[&bundle])))
        }
        Install::WindowsInstaller => {
            // The installer closes Quarry, upgrades the install it finds and starts Quarry again.
            let mut cmd = Command::new(package);
            cmd.args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/RELAUNCH=1"]);
            Ok(Finish::Restart(cmd))
        }
        Install::Portable(dir) => {
            let stage = work.join("unpacked");
            let _ = std::fs::remove_dir_all(&stage);
            std::fs::create_dir_all(&stage)?;
            // tar unpacks both the Linux tarball and, on Windows 10 and later, the ZIP.
            run(Command::new("tar").arg(if cfg!(windows) { "-xf" } else { "-xzf" }).arg(package).arg("-C").arg(&stage))?;
            // The tarball has one top folder; the ZIP has the files at its root.
            let root = match std::fs::read_dir(&stage)?.filter_map(Result::ok).collect::<Vec<_>>().as_slice() {
                [only] if only.path().is_dir() => only.path(),
                _ => stage.clone(),
            };
            let exe = format!("quarry{}", std::env::consts::EXE_SUFFIX);
            if !root.join(&exe).is_file() {
                bail!("the downloaded package does not contain {exe}");
            }
            replace_tree(&root, dir)?;
            let exe = dir.join(&exe);
            if cfg!(windows) {
                Ok(Finish::Restart(Command::new(exe)))
            } else {
                Ok(Finish::Restart(after_exit(&exe.to_string_lossy(), &[])))
            }
        }
        Install::LinuxPackage => {
            let downloads = dirs_downloads().unwrap_or_else(|| work.to_path_buf());
            let target = downloads.join(package.file_name().unwrap_or_default());
            std::fs::copy(package, &target)?;
            let _ = Command::new("xdg-open").arg(&target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            Ok(Finish::Message(format!(
                "The update was saved to {}. Install it with your software installer, or run:\n\nsudo apt install {}",
                target.display(),
                target.display()
            )))
        }
        Install::Development => bail!("development builds are not updated in place"),
    }
}

fn dirs_downloads() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let d = home.join("Downloads");
    d.is_dir().then_some(d)
}

/// Copies every file of `from` over `to`. Files in use are renamed to
/// `*.old` first (allowed for running executables on Windows and Linux) and
/// removed at the next start.
fn replace_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            std::fs::create_dir_all(&dst)?;
            replace_tree(&src, &dst)?;
            continue;
        }
        if dst.exists() {
            let old = old_name(&dst);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&dst, &old).with_context(|| format!("could not replace {}", dst.display()))?;
        }
        std::fs::copy(&src, &dst).with_context(|| format!("could not write {}", dst.display()))?;
    }
    Ok(())
}

fn old_name(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    path.with_file_name(name)
}

/// Removes what an earlier update left behind: the downloaded packages
/// (the Windows installer still needed its file after Quarry quit) and the
/// `*.old` files of a portable update.
pub fn cleanup_after_update() {
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("quarry-update-") {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    if let Install::Portable(dir) = detect_install() {
        let _ = remove_old(&dir);
    }
}

fn remove_old(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            let _ = remove_old(&path);
        } else if path.extension().is_some_and(|e| e == "old") {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

// ---- state shown in the UI ------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Arc<Release>),
    Downloading { version: String, done: u64, total: u64 },
    Installing(String),
    Failed(String),
}

#[derive(Default)]
pub struct Updates {
    pub status: Status,
}

impl Global for Updates {}

impl Updates {
    pub fn status(cx: &App) -> Status {
        cx.try_global::<Updates>().map(|u| u.status.clone()).unwrap_or_default()
    }

    /// The release that can be installed, if any.
    pub fn available(cx: &App) -> Option<Arc<Release>> {
        match Self::status(cx) {
            Status::Available(r) => Some(r),
            _ => None,
        }
    }

    /// A short line for buttons and banners, or `None` when idle.
    pub fn label(cx: &App) -> Option<String> {
        match Self::status(cx) {
            Status::Available(r) => Some(format!("Update to {}", r.version().map(|v| v.to_string()).unwrap_or(r.tag_name.clone()))),
            Status::Downloading { done, total, .. } if total > 0 => Some(format!("Downloading update… {}%", done * 100 / total)),
            Status::Downloading { .. } => Some("Downloading update…".into()),
            Status::Installing(_) => Some("Installing update…".into()),
            _ => None,
        }
    }
}

/// The "Update to x.y.z" pill shown in title bars while an update is
/// available or installing; `None` otherwise.
pub fn pill(cx: &App) -> Option<gpui_kit::AnyElement> {
    use gpui_kit::assets::IconName;
    use gpui_kit::prelude::FluentBuilder as _;
    use gpui_kit::*;
    use crate::theme::{PaletteExt as _, UI_TEXT};
    let label = Updates::label(cx)?;
    let p = cx.palette();
    let ready = Updates::available(cx).is_some();
    Some(
        gpui_kit::component::h_flex()
            .id("update-pill")
            // Keep presses from turning into window drags in the title bar.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .h(px(28.))
            .px(px(12.))
            .gap(px(6.))
            .flex_shrink_0()
            .rounded(px(14.))
            .bg(p.accent)
            .text_color(white())
            .text_size(UI_TEXT)
            .font_weight(FontWeight::MEDIUM)
            .child(crate::widgets::icon(IconName::CircleArrowUp, 13., white()))
            .child(label)
            .when(ready, |d| {
                d.hover(|s| s.opacity(0.9)).on_click(|_, window, cx| {
                    if let Some(r) = Updates::available(cx) {
                        offer(r, window, cx);
                    }
                })
            })
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(if ready { "Install the new version and restart" } else { "Updating Quarry" }).build(window, cx)
            })
            .into_any_element(),
    )
}

fn set_status(status: Status, cx: &mut App) {
    cx.set_global(Updates { status });
    cx.refresh_windows();
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Starts the background check at launch, when enabled and due.
pub fn init(cx: &mut App) {
    cx.set_global(Updates::default());
    cleanup_after_update();
    let settings = &AppState::get(cx).settings;
    let due = now().saturating_sub(settings.last_update_check) >= CHECK_EVERY.as_secs();
    if !settings.check_updates || !due {
        return;
    }
    cx.spawn(async move |cx| {
        // Let the first window settle before going online.
        cx.background_executor().timer(Duration::from_secs(4)).await;
        let found = db(latest_release()).await;
        let _ = cx.update(|cx| {
            cx.update_global::<AppState, _>(|s, _| {
                s.settings.last_update_check = now();
                s.settings.save();
            });
            if let Ok(release) = found {
                if release.version().is_some_and(|v| v > current_version()) {
                    set_status(Status::Available(Arc::new(release)), cx);
                }
            }
        });
    })
    .detach();
}

/// "Check for updates…": checks now and offers the update, or says there is none.
pub fn check_now(window: &mut Window, cx: &mut App) {
    if matches!(Updates::status(cx), Status::Checking | Status::Downloading { .. } | Status::Installing(_)) {
        return;
    }
    set_status(Status::Checking, cx);
    window
        .spawn(cx, async move |cx| {
            let found = db(latest_release()).await;
            let _ = cx.update(|window, cx| {
                cx.update_global::<AppState, _>(|s, _| {
                    s.settings.last_update_check = now();
                    s.settings.save();
                });
                match found {
                    Ok(release) if release.version().is_some_and(|v| v > current_version()) => {
                        let release = Arc::new(release);
                        set_status(Status::Available(release.clone()), cx);
                        offer(release, window, cx);
                    }
                    Ok(_) => {
                        set_status(Status::UpToDate, cx);
                        prompt::info("No update available", &format!("Quarry {} is the latest version.", current_version()), window, cx);
                    }
                    Err(e) => {
                        set_status(Status::Failed(format!("{e:#}")), cx);
                        prompt::error("Could not check for updates", &format!("{e:#}"), window, cx);
                    }
                }
            });
        })
        .detach();
}

/// Asks before installing `release`.
pub fn offer(release: Arc<Release>, window: &mut Window, cx: &mut App) {
    let version = release.version().map(|v| v.to_string()).unwrap_or(release.tag_name.clone());
    let install = detect_install();
    let size = asset_name(&install, release.version().unwrap_or(Version(0, 0, 0)))
        .and_then(|n| release.asset(&n).map(|a| a.size))
        .map(|s| format!(" ({:.1} MB)", s as f64 / 1_048_576.0))
        .unwrap_or_default();
    let (text, ok) = match &install {
        Install::Development => (
            format!("Quarry {version} is available. This is a development build, so it is not updated in place; the release page opens instead."),
            "Open release page",
        ),
        Install::LinuxPackage => (
            format!("Quarry {version} is available{size}. Quarry will download the package and open it in your software installer."),
            "Download",
        ),
        _ => (
            format!(
                "Quarry {version} is available (you have {}). Quarry will download it{size}, verify it, install it and restart.\n\nCommit or discard unsaved changes first.",
                current_version()
            ),
            "Update and restart",
        ),
    };
    prompt::confirm("Update available", &text, ok, window, cx, move |window, cx| {
        if install == Install::Development {
            cx.open_url(&release.html_url);
        } else {
            start(release.clone(), window, cx);
        }
    });
}

/// Opens the release notes of the available update in the browser.
pub fn open_notes(cx: &mut App) {
    if let Some(r) = Updates::available(cx) {
        cx.open_url(&r.html_url);
    } else {
        cx.open_url(&format!("https://github.com/{REPO}/releases/latest"));
    }
}

/// Downloads, verifies and installs `release`, then restarts Quarry.
fn start(release: Arc<Release>, window: &mut Window, cx: &mut App) {
    let install = detect_install();
    let Some(version) = release.version() else { return };
    let Some(asset) = asset_name(&install, version).and_then(|n| release.asset(&n).cloned()) else {
        let msg = format!("The {} release has no package for this system. Download it from {}", release.tag_name, release.html_url);
        set_status(Status::Failed(msg.clone()), cx);
        return prompt::error("Update failed", &msg, window, cx);
    };
    let label = version.to_string();
    set_status(Status::Downloading { version: label.clone(), done: 0, total: asset.size }, cx);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
    {
        let label = label.clone();
        cx.spawn(async move |cx| {
            while let Some((done, total)) = rx.recv().await {
                let _ = cx.update(|cx| set_status(Status::Downloading { version: label.clone(), done, total }, cx));
            }
        })
        .detach();
    }
    window
        .spawn(cx, async move |cx| {
            let work = std::env::temp_dir().join(format!("quarry-update-{version}"));
            let (r2, w2, i2) = (release.clone(), work.clone(), install.clone());
            let downloaded = db(async move {
                let mut last = 0u64;
                download(&r2, &asset, &w2, move |done, total| {
                    // About every 1%, not every chunk.
                    if total == 0 || done == total || done.saturating_sub(last) * 100 >= total {
                        last = done;
                        let _ = tx.send((done, total));
                    }
                })
                .await
            })
            .await;
            let installed = match downloaded {
                Ok(package) => {
                    let _ = cx.update(|_, cx| set_status(Status::Installing(label.clone()), cx));
                    db(async move { tokio::task::spawn_blocking(move || install_package(&i2, &package, &work)).await? }).await
                }
                Err(e) => Err(e),
            };
            let _ = cx.update(|window, cx| match installed {
                Ok(Finish::Restart(mut cmd)) => match cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                    Ok(_) => cx.quit(),
                    Err(e) => {
                        set_status(Status::Idle, cx);
                        prompt::info("Update installed", &format!("Quarry {label} is installed. Restart Quarry to use it. ({e})"), window, cx);
                    }
                },
                Ok(Finish::Message(msg)) => {
                    set_status(Status::Idle, cx);
                    prompt::info("Update downloaded", &msg, window, cx);
                }
                Err(e) => {
                    let msg = format!("{e:#}\n\nYou can download Quarry {label} from {}", release.html_url);
                    set_status(Status::Failed(msg.clone()), cx);
                    prompt::error("Update failed", &msg, window, cx);
                }
            });
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn versions() {
        assert_eq!(Version::parse("v0.2.0"), Some(Version(0, 2, 0)));
        assert_eq!(Version::parse("1.10"), Some(Version(1, 10, 0)));
        assert_eq!(Version::parse("1.2.3-beta.1"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("x1"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version(0, 2, 1).to_string(), "0.2.1");
    }

    #[::core::prelude::v1::test]
    fn checksums() {
        let sums = parse_checksums(&format!("{}  Quarry-0.2.0-macos-arm64.zip\n{} *QuarrySetup-0.2.0-x64.exe\nnot a line\n", "A".repeat(64), "b".repeat(64)));
        assert_eq!(sums.get("Quarry-0.2.0-macos-arm64.zip").map(String::as_str), Some("a".repeat(64).as_str()));
        assert_eq!(sums.get("QuarrySetup-0.2.0-x64.exe").map(String::as_str), Some("b".repeat(64).as_str()));
        assert_eq!(sums.len(), 2);
    }

    #[::core::prelude::v1::test]
    fn packages_match_the_release_names() {
        let v = Version(0, 2, 0);
        let a = arch();
        assert_eq!(asset_name(&Install::MacApp("/Applications/Quarry.app".into()), v).unwrap(), format!("Quarry-0.2.0-macos-{a}.zip"));
        assert_eq!(asset_name(&Install::WindowsInstaller, v).unwrap(), format!("QuarrySetup-0.2.0-{a}.exe"));
        assert_eq!(asset_name(&Install::LinuxPackage, v).unwrap(), format!("Quarry-0.2.0-linux-{a}.deb"));
        assert_eq!(asset_name(&Install::Development, v), None);
    }

    #[::core::prelude::v1::test]
    fn development_builds_are_recognized() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(detect_install_at(&exe), Install::Development);
        if cfg!(target_os = "macos") {
            let app = Path::new("/Applications/Quarry.app/Contents/MacOS/quarry");
            assert_eq!(detect_install_at(app), Install::MacApp("/Applications/Quarry.app".into()));
        }
    }

    #[::core::prelude::v1::test]
    fn portable_files_are_replaced_and_cleaned_up() {
        let root = std::env::temp_dir().join(format!("quarry-update-test-{}", std::process::id()));
        let (from, to) = (root.join("new"), root.join("app"));
        std::fs::create_dir_all(from.join("docs")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("quarry"), "new").unwrap();
        std::fs::write(from.join("docs/AI.md"), "doc").unwrap();
        std::fs::write(to.join("quarry"), "old").unwrap();
        replace_tree(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("quarry")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(to.join("quarry.old")).unwrap(), "old");
        assert!(to.join("docs/AI.md").exists());
        remove_old(&to).unwrap();
        assert!(!to.join("quarry.old").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
