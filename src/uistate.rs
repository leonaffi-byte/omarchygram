//! Window/pane state: `~/.local/state/omarchygram/ui-state.toml`.
//! Orchestrator-owned. Not secret, not hot-reloaded, not user-edited —
//! the UI restores it at start and saves it (debounced ≤1/s) on changes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiState {
    pub window_w: i32,
    pub window_h: i32,
    pub maximized: bool,
    pub sidebar_width: i32,
    /// The user's explicit choice; auto-collapse on narrow windows never writes this.
    pub sidebar_collapsed: bool,
    pub info_panel_open: bool,
    /// Info panel width when shown as a third column.
    pub info_width: i32,
    /// Selected folder tab; 0 = All.
    pub folder_id: i32,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            window_w: 1100,
            window_h: 720,
            maximized: false,
            sidebar_width: 300,
            sidebar_collapsed: false,
            info_panel_open: false,
            info_width: 320,
            folder_id: 0,
        }
    }
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("OMG_UISTATE_PATH") {
        return PathBuf::from(p);
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .expect("cannot determine XDG state dir — is HOME set?")
        .join("omarchygram/ui-state.toml")
}

impl UiState {
    /// Missing or invalid → defaults, with out-of-range values clamped.
    pub fn load() -> UiState {
        let mut s = std::fs::read_to_string(path())
            .ok()
            .and_then(|t| toml::from_str::<UiState>(&t).ok())
            .unwrap_or_default();
        s.window_w = s.window_w.clamp(480, 10000);
        s.window_h = s.window_h.clamp(360, 10000);
        s.sidebar_width = s.sidebar_width.clamp(220, 2000);
        s.info_width = s.info_width.clamp(280, 2000);
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        use std::io::Write;
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = p.with_extension("toml.tmp");
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &p)
    }
}
