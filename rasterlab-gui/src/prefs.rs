//! GUI preference persistence.
//!
//! Prefs are stored as YAML in the platform-appropriate user data directory:
//!   * macOS   – `~/Library/Application Support/rasterlab/prefs.yaml`
//!   * Linux   – `~/.local/share/rasterlab/prefs.yaml`
//!   * Windows – `%APPDATA%\rasterlab\prefs.yaml`

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// User-selectable theme preference, stored in the prefs file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePref {
    /// Follow the OS dark/light setting (default).
    #[default]
    System,
    Dark,
    Light,
}

impl ThemePref {
    pub fn to_egui(self) -> egui::ThemePreference {
        match self {
            Self::System => egui::ThemePreference::System,
            Self::Dark => egui::ThemePreference::Dark,
            Self::Light => egui::ThemePreference::Light,
        }
    }
}

/// Maximum number of paths kept in the recently-opened list.
const MAX_RECENT: usize = 10;

/// Persistent GUI preferences.  Missing keys are treated as `false`/default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Prefs {
    /// Open/closed state of each collapsing tool section, keyed by a stable
    /// ASCII string (e.g. `"blur"`, `"crop"`, `"hsl_panel"`).
    #[serde(default)]
    pub tools_open: HashMap<String, bool>,
    /// User's preferred theme.  Defaults to `System` (follow OS setting).
    #[serde(default)]
    pub theme: ThemePref,
    /// Use the native OS file dialog instead of the built-in egui one.
    /// The built-in dialog works over waypipe and other network display
    /// protocols where the native dialog is invisible.
    /// Defaults to `true` (native) since that is the better experience on
    /// local desktops.
    #[serde(default = "default_true")]
    pub use_native_dialogs: bool,
    /// Most-recently-opened files, newest first.  Capped at [`MAX_RECENT`].
    #[serde(default)]
    pub recent_files: Vec<PathBuf>,
    /// UI scale override (pixels-per-point).  `None` means follow the OS/
    /// display DPI automatically.  Stored values are restricted to the set
    /// [0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0].
    #[serde(default)]
    pub ui_scale: Option<f32>,

    /// Default file-type filter for the Open dialog (e.g. `"All supported"`,
    /// `"Images"`, `"Camera RAW"`).  `None` means the "All Files" catch-all.
    #[serde(default = "default_open_file_filter")]
    pub open_file_filter: Option<String>,

    /// JPEG export quality (1–100).  Matches `EncodeOptions::jpeg_quality`.
    #[serde(default = "default_jpeg_quality")]
    pub jpeg_quality: u8,
    /// PNG export compression level (0–9).  Matches `EncodeOptions::png_compression`.
    #[serde(default = "default_png_compression")]
    pub png_compression: u8,
    /// Whether to copy EXIF metadata into exported files.
    #[serde(default = "default_true")]
    pub preserve_metadata: bool,

    // ── Library ──────────────────────────────────────────────────────────────
    /// Most-recently-opened libraries, newest first.  Capped at [`MAX_RECENT`].
    #[serde(default)]
    pub recent_libraries: Vec<PathBuf>,
    /// The library that was open when the app last exited.
    #[serde(default)]
    pub last_library: Option<PathBuf>,
    /// Thumbnail display scale in the library grid (0.25–1.0; 1.0 = 512px).
    #[serde(default = "default_thumb_scale")]
    pub library_thumb_scale: f32,
}

fn default_true() -> bool {
    true
}

fn default_open_file_filter() -> Option<String> {
    Some("All supported".to_string())
}

fn default_jpeg_quality() -> u8 {
    90
}

fn default_png_compression() -> u8 {
    6
}

fn default_thumb_scale() -> f32 {
    0.5
}

impl Prefs {
    /// Returns the platform-specific path for the prefs file, or `None` if the
    /// data directory cannot be determined.
    pub fn path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("rasterlab").join("prefs.yaml"))
    }

    /// Load prefs from disk.  Returns an all-default `Prefs` (all tools
    /// collapsed) if the file does not exist or cannot be parsed.
    pub fn load() -> Self {
        let path = match Self::path() {
            Some(p) => p,
            None => return Self::default(),
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };
        serde_yaml::from_str(&content).unwrap_or_default()
    }

    /// Persist prefs to disk, creating the directory if necessary.
    pub fn save(&self) {
        let path = match Self::path() {
            Some(p) => p,
            None => return,
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(yaml) = serde_yaml::to_string(self) {
            let _ = std::fs::write(&path, yaml);
        }
    }

    /// Returns `true` if the tool section with this key is currently open.
    /// Defaults to `false` for unknown keys.
    pub fn is_tool_open(&self, key: &str) -> bool {
        self.tools_open.get(key).copied().unwrap_or(false)
    }

    /// Prepend `path` to the recent-files list, deduplicating and capping at
    /// [`MAX_RECENT`].
    pub fn push_recent(&mut self, path: PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(MAX_RECENT);
    }

    /// Prepend `path` to the recent-libraries list, deduplicating and capping
    /// at [`MAX_RECENT`].
    pub fn push_recent_library(&mut self, path: PathBuf) {
        self.recent_libraries.retain(|p| p != &path);
        self.recent_libraries.insert(0, path);
        self.recent_libraries.truncate(MAX_RECENT);
    }
}
