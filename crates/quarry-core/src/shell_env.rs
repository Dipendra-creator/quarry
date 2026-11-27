//! The PATH programs started by Quarry should see.
//!
//! Apps opened from the macOS Finder or Dock, or from a Linux desktop
//! launcher, get the system's bare PATH (`/usr/bin:/bin:/usr/sbin:/sbin`),
//! not the one the user's shell builds. CLIs installed with npm, Homebrew,
//! pipx or their own installers (`claude`, `codex`, `pg_dump`, `sqlcmd`) are
//! then not found. Quarry asks the user's login shell for its PATH once and
//! adds the usual install folders, as terminals and editors do.

use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long the login shell may take before Quarry gives up on it.
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);
const MARKER: &str = "__QUARRY_PATH__";

/// PATH for finding and starting external programs. Computed on first use.
pub fn user_path() -> &'static OsString {
    static PATH: OnceLock<OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let current = std::env::var_os("PATH").unwrap_or_default();
        if cfg!(windows) {
            // Windows apps get the user's PATH from the registry already.
            return current;
        }
        let shell = login_shell_path();
        merge(shell.as_deref(), &current, &common_dirs())
    })
}

/// The first `name` (with Windows' executable extensions) on [`user_path`].
pub fn find_program(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &["exe", "cmd", "bat", ""] } else { &[""] };
    std::env::split_paths(user_path())
        .flat_map(|dir| exts.iter().map(move |e| dir.join(name).with_extension(e)))
        .find(|p| p.is_file())
}

/// Shell entries first, then the inherited ones, then known install folders
/// that exist; each folder once.
fn merge(shell: Option<&std::ffi::OsStr>, current: &std::ffi::OsStr, extra: &[PathBuf]) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut add = |d: PathBuf| {
        if !d.as_os_str().is_empty() && !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    if let Some(shell) = shell {
        std::env::split_paths(shell).for_each(&mut add);
    }
    std::env::split_paths(current).for_each(&mut add);
    extra.iter().filter(|d| d.is_dir()).cloned().for_each(add);
    std::env::join_paths(dirs).unwrap_or_else(|_| current.to_os_string())
}

/// Where package managers and CLI installers put executables.
fn common_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin", "/opt/local/bin", "/snap/bin"]
        .iter()
        .map(PathBuf::from)
        .collect();
    if let Some(home) = dirs::home_dir() {
        for rel in [".local/bin", ".npm-global/bin", ".claude/local", ".bun/bin", ".volta/bin", ".cargo/bin", ".deno/bin", "bin"] {
            dirs.push(home.join(rel));
        }
    }
    dirs
}

/// `$PATH` as the user's interactive login shell sets it, or `None` when the
/// shell is missing, fails or takes too long.
fn login_shell_path() -> Option<OsString> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" }.into());
    if !Path::new(&shell).is_file() {
        return None;
    }
    // Interactive too: many people extend PATH in ~/.zshrc or ~/.bashrc only.
    // The markers separate the value from anything the profile prints.
    let mut child = Command::new(&shell)
        .args(["-l", "-i", "-c", &format!("printf '{MARKER}%s{MARKER}' \"$PATH\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < SHELL_TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    extract(&out).map(OsString::from)
}

/// The text between the first two markers.
fn extract(output: &str) -> Option<String> {
    let start = output.find(MARKER)? + MARKER.len();
    let len = output[start..].find(MARKER)?;
    Some(output[start..start + len].to_string()).filter(|p| !p.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_cut_out_profile_noise() {
        assert_eq!(extract("welcome!\n__QUARRY_PATH__/a:/b__QUARRY_PATH__\nbye").as_deref(), Some("/a:/b"));
        assert_eq!(extract("no markers"), None);
        assert_eq!(extract("__QUARRY_PATH____QUARRY_PATH__"), None);
    }

    #[cfg(unix)]
    #[test]
    fn shell_entries_come_first_without_duplicates() {
        let tmp = std::env::temp_dir();
        let merged = merge(
            Some(std::ffi::OsStr::new("/shell/bin:/usr/bin")),
            std::ffi::OsStr::new("/usr/bin:/bin"),
            &[tmp.clone(), PathBuf::from("/does/not/exist")],
        );
        let dirs: Vec<PathBuf> = std::env::split_paths(&merged).collect();
        assert_eq!(dirs, vec![PathBuf::from("/shell/bin"), PathBuf::from("/usr/bin"), PathBuf::from("/bin"), tmp]);
    }

    #[cfg(unix)]
    #[test]
    fn finds_programs_on_the_user_path() {
        // `sh` is on every Unix PATH, the bare system one included.
        assert!(find_program("sh").is_some());
        assert!(find_program("surely-not-a-real-program-quarry").is_none());
    }
}
