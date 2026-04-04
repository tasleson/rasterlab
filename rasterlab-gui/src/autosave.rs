//! Automatic pipeline state persistence.
//!
//! After each edit the active pipeline state is written to a small JSON file in
//! the platform data directory.  If the user quits without saving, or the app
//! crashes, these files are listed under File → Previous Unsaved Work and can
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
#[derive(Serialize, Deserialize)]
pub struct AutosaveFile {
    /// Absolute path of the source image on disk (used when restoring).
    pub source_path: String,
    /// Absolute path of the `.rlab` project file, if one was open.
    /// Used for display only — restoring always reopens `source_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
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
pub struct AutosaveEntry {
    pub data: AutosaveFile,
}

/// Write (or overwrite) the autosave file for `session_id`.
///
/// `project_path` should be `Some` when the user has a `.rlab` project open;
/// it is stored for display purposes only (restore always reopens `source_path`).
///
/// Silently returns without writing if the autosave directory cannot be
/// created or the data cannot be serialised.
pub fn write(
    session_id: u64,
    source_path: &std::path::Path,
    project_path: Option<&std::path::Path>,
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

/// Delete all autosave files from the autosave directory.
pub fn delete_all() {
    let Some(dir) = autosave_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "json") {
            let _ = std::fs::remove_file(path);
        }
    }
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
    result.sort_by(|a, b| b.data.started_at.cmp(&a.data.started_at));
    result
}

/// Returns the filename to show in the menu for this entry.
///
/// Prefers the `.rlab` project filename when available; falls back to the
/// source image filename.
pub fn display_name(data: &AutosaveFile) -> String {
    let path_str = data.project_path.as_deref().unwrap_or(&data.source_path);
    std::path::Path::new(path_str)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_str.to_owned())
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
