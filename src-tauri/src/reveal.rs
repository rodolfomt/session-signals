//! Open a local folder in the OS file manager. Used by Settings to take the
//! user to the `alerts/` folder, which lives in the app-data dir and is not
//! a path anyone can be expected to type.
//!
//! No shell: each platform's file manager is invoked directly with the path
//! as a single argument, mirroring `cli_alert`'s spawn discipline. The path
//! is always one we produced (`cli_alert::alerts_dir()`), never user input.

use std::path::Path;
use std::process::{Command, Stdio};

/// The file-manager binary for this platform.
fn file_manager() -> &'static str {
    if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Open `dir` in the file manager. Returns an error string suitable for a
/// UI toast. Fire-and-forget: we never wait on the file manager, which on
/// some platforms outlives the click by the length of the user's session.
pub fn reveal_dir(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} does not exist", dir.display()));
    }
    Command::new(file_manager())
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open folder: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("beacon-reveal-test-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_manager_is_platform_appropriate() {
        let expected = if cfg!(target_os = "windows") {
            "explorer"
        } else if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        assert_eq!(file_manager(), expected);
    }

    #[test]
    fn reveal_dir_rejects_a_missing_path() {
        let dir = scratch_dir("missing-parent").join("does-not-exist");
        assert!(reveal_dir(&dir).is_err());
    }

    #[test]
    fn reveal_dir_rejects_a_file() {
        let dir = scratch_dir("file-not-dir");
        let file = dir.join("not-a-dir.txt");
        std::fs::write(&file, "").unwrap();
        assert!(reveal_dir(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
