//! `<cache>/bin/tools.json`: what jc installed, so it knows when to update.

use crate::paths;
use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ToolState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yt_dlp: Option<YtDlpState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg: Option<FfmpegState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YtDlpState {
    /// Release tag, which is also the version (for example `2026.10.26`).
    pub version: String,
    pub sha256: String,
    pub installed_at: Timestamp,
    /// Last time jc asked GitHub for a newer release.
    pub checked_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfmpegState {
    /// [`super::platform::FfmpegBuild::id`] of the installed build.
    pub build: String,
    pub installed_at: Timestamp,
}

fn path(bin_dir: &Path) -> PathBuf {
    bin_dir.join("tools.json")
}

impl ToolState {
    /// A missing or unreadable file means nothing is recorded; doctor then
    /// re-checks or reinstalls, which is always safe.
    pub fn load(bin_dir: &Path) -> Self {
        std::fs::read_to_string(path(bin_dir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, bin_dir: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).context("serializing tools.json")?;
        paths::write_atomic(&path(bin_dir), text.as_bytes())
    }
}
