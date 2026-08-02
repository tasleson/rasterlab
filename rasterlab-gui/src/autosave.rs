//! Automatic pipeline state persistence.
//!
//! After each edit the active pipeline state is written to a small JSON file in
//! the platform data directory.  If the user quits without saving, or the app
//! crashes, these files are listed under File → Previously Unsaved Work and can
//! be restored.
//!
//! Files live in:
//!   * macOS   – `~/Library/Application Support/rasterlab/autosave/`
//!   * Linux   – `~/.local/share/rasterlab/autosave/`
//!   * Windows – `%APPDATA%\rasterlab\autosave\`
//!
//! Each editing session produces one file named `{session_id}.json` where
//! `session_id` is the Unix timestamp of when the source image was opened.
//! Restoring from a session reuses the same `session_id` so the file is
//! correctly cleaned up when the user eventually saves.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rasterlab_core::project::SavedCopy;
use serde::{Deserialize, Serialize};

/// Returns the platform-specific autosave directory, or `None` if unavailable.
pub fn autosave_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("rasterlab").join("autosave"))
}

/// Returns the current Unix timestamp in seconds.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Contents of one autosave file.
#[derive(Clone, Serialize, Deserialize)]
pub struct AutosaveFile {
    /// Absolute path of the source image on disk (used when restoring).
    pub source_path: String,
    /// Absolute path of the `.rlab` project file, if one was open.
    /// Preferred restore target when present; also used for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    /// Friendly filename captured when the autosave was written. Library
    /// projects are content-addressed, so their project basename is a hash
    /// rather than the original imported filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Unix timestamp when this editing session started (also the filename stem).
    pub started_at: u64,
    /// Unix timestamp of the last autosave write.
    pub saved_at: u64,
    /// Index of the active virtual copy at the time of the autosave.
    pub active_copy: usize,
    /// Pipeline states for all virtual copies.
    pub copies: Vec<SavedCopy>,
}

/// A parsed autosave entry ready to display in the UI.
#[derive(Clone)]
pub struct AutosaveEntry {
    pub data: AutosaveFile,
}

/// Write (or overwrite) the autosave file for `session_id`.
///
/// `project_path` should be `Some` when the user has a `.rlab` project open;
/// restore prefers it so autosaved project edits do not depend on the original
/// source path still existing.
///
/// Silently returns without writing if the autosave directory cannot be
/// created or the data cannot be serialised.
pub fn write(
    session_id: u64,
    source_path: &std::path::Path,
    project_path: Option<&std::path::Path>,
    display_name: Option<&str>,
    copies: &[SavedCopy],
    active: usize,
) {
    let Some(dir) = autosave_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let file = AutosaveFile {
        source_path: source_path.to_string_lossy().into_owned(),
        project_path: project_path.map(|p| p.to_string_lossy().into_owned()),
        display_name: display_name.map(str::to_owned),
        started_at: session_id,
        saved_at: unix_now(),
        active_copy: active,
        copies: copies.to_vec(),
    };
    let Ok(json) = serde_json::to_string_pretty(&file) else {
        return;
    };
    let path = dir.join(format!("{}.json", session_id));
    let _ = std::fs::write(path, json);
}

/// Delete the autosave file for `session_id` after a successful project save.
pub fn delete(session_id: u64) {
    let Some(dir) = autosave_dir() else { return };
    let _ = std::fs::remove_file(dir.join(format!("{}.json", session_id)));
}

/// Scan the autosave directory and return all valid entries, newest first.
pub fn list_entries() -> Vec<AutosaveEntry> {
    let Some(dir) = autosave_dir() else {
        return Vec::new();
    };
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut result: Vec<AutosaveEntry> = read_dir
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let content = std::fs::read_to_string(e.path()).ok()?;
            let data: AutosaveFile = serde_json::from_str(&content).ok()?;
            Some(AutosaveEntry { data })
        })
        .collect();

    // Newest session first.
    result.sort_by_key(|a| std::cmp::Reverse(a.data.started_at));
    result
}

/// Returns the filename to show in the menu for this entry.
///
/// Prefers the friendly name captured in the autosave. For autosaves written
/// before that field existed, a content-addressed project name falls back to
/// the source image filename; ordinary projects still use their `.rlab` name.
pub fn display_name(data: &AutosaveFile) -> String {
    if let Some(name) = data.display_name.as_deref().filter(|name| !name.is_empty()) {
        return name.to_owned();
    }

    let project_path = data.project_path.as_deref();
    let path_str = match project_path {
        Some(path) if !has_content_hash_stem(std::path::Path::new(path)) => path,
        _ => &data.source_path,
    };
    std::path::Path::new(path_str)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_str.to_owned())
}

/// Library project files use a lowercase BLAKE3 hash as their filename stem.
fn has_content_hash_stem(path: &std::path::Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Human-readable description of when an autosave was last written relative to now.
///
/// Examples: `"just now"`, `"5 min ago"`, `"3 hr ago"`, `"2 days ago"`.
pub fn format_age(saved_at: u64) -> String {
    let age = unix_now().saturating_sub(saved_at);
    match age {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", age / 60),
        3600..=86399 => format!("{} hr ago", age / 3600),
        _ => format!("{} days ago", age / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn autosave(source_path: &str, project_path: Option<&str>) -> AutosaveFile {
        AutosaveFile {
            source_path: source_path.to_owned(),
            project_path: project_path.map(str::to_owned),
            display_name: None,
            started_at: 1,
            saved_at: 2,
            active_copy: 0,
            copies: Vec::new(),
        }
    }

    #[test]
    fn captured_display_name_takes_precedence() {
        let mut data = autosave(
            "/imports/NUB_0483.NEF",
            Some(
                "/library/objects/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.rlab",
            ),
        );
        data.display_name = Some("NUB_0483.NEF".to_owned());

        assert_eq!(display_name(&data), "NUB_0483.NEF");
    }

    #[test]
    fn legacy_library_autosave_uses_source_filename_instead_of_hash() {
        let data = autosave(
            "/imports/NUB_0483.NEF",
            Some(
                "/library/objects/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.rlab",
            ),
        );

        assert_eq!(display_name(&data), "NUB_0483.NEF");
    }

    #[test]
    fn legacy_named_project_keeps_project_filename() {
        let data = autosave("/photos/NUB_0483.NEF", Some("/projects/airshow.rlab"));

        assert_eq!(display_name(&data), "airshow.rlab");
    }

    #[test]
    fn old_json_without_display_name_still_deserializes() {
        let json = r#"{
            "source_path": "/photos/NUB_0483.NEF",
            "project_path": "/projects/airshow.rlab",
            "started_at": 1,
            "saved_at": 2,
            "active_copy": 0,
            "copies": []
        }"#;

        let data: AutosaveFile = serde_json::from_str(json).unwrap();
        assert_eq!(data.display_name, None);
        assert_eq!(display_name(&data), "airshow.rlab");
    }
}
