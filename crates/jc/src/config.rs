//! Optional `<workspace>/config.toml`. Every field has a default, so the file
//! may be missing or contain only what the user wants to change:
//!
//! ```toml
//! [tools]
//! ffmpeg = "/opt/ffmpeg/bin/ffmpeg"   # ffprobe is looked up next to it
//! prefer_system_ffmpeg = false        # always use jc's own ffmpeg build
//!
//! [doctor]
//! yt_dlp_max_age_days = 3
//!
//! [models]
//! whisper = "small"                   # faster, less accurate
//!
//! [transcribe]
//! threads = 4
//!
//! [ingest]
//! max_height = 1440
//!
//! [chat]
//! twitch_delay_s = 6.0
//! ```

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub tools: ToolsConfig,
    pub doctor: DoctorConfig,
    pub models: ModelsConfig,
    pub transcribe: TranscribeConfig,
    pub ingest: IngestConfig,
    pub chat: ChatConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// Use this ffmpeg instead of a managed or system one.
    pub ffmpeg: Option<PathBuf>,
    /// Defaults to the ffprobe next to `ffmpeg`.
    pub ffprobe: Option<PathBuf>,
    /// Use this yt-dlp instead of the managed one. jc cannot update it.
    pub yt_dlp: Option<PathBuf>,
    /// Use this deno (yt-dlp's JavaScript runtime for YouTube) instead of the
    /// managed one.
    pub deno: Option<PathBuf>,
    /// Use an ffmpeg found on PATH when it has every feature jc needs,
    /// instead of downloading one.
    pub prefer_system_ffmpeg: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            ffmpeg: None,
            ffprobe: None,
            yt_dlp: None,
            deno: None,
            prefer_system_ffmpeg: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DoctorConfig {
    /// Check for a new yt-dlp release when the last check is older than this.
    pub yt_dlp_max_age_days: u32,
}

impl Default for DoctorConfig {
    fn default() -> Self {
        Self {
            yt_dlp_max_age_days: 7,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ModelsConfig {
    /// Whisper model: `tiny`, `base`, `small`, `large-v3-turbo-q5_0` or
    /// `large-v3-turbo`.
    pub whisper: String,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            whisper: "large-v3-turbo-q5_0".into(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TranscribeConfig {
    /// CPU threads for whisper. 0 = one per core, up to 8.
    pub threads: u32,
    /// 1 = greedy decoding (fast). Higher values use beam search, which is
    /// slightly more accurate and several times slower.
    pub beam_size: u32,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct IngestConfig {
    /// Highest video resolution to download.
    pub max_height: u32,
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self { max_height: 1080 }
    }
}

/// Viewers react a few seconds after something happens. Chat is shifted back
/// by this much so reactions line up with their cause.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ChatConfig {
    pub twitch_delay_s: f64,
    pub youtube_delay_s: f64,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            twitch_delay_s: 5.0,
            youtube_delay_s: 4.0,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("reading {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.tools.prefer_system_ffmpeg);
        assert_eq!(config.doctor.yt_dlp_max_age_days, 7);
    }

    #[test]
    fn partial_sections_keep_other_defaults() {
        let config: Config = toml::from_str("[tools]\nffmpeg = \"/x/ffmpeg\"\n").unwrap();
        assert_eq!(config.tools.ffmpeg.as_deref(), Some(Path::new("/x/ffmpeg")));
        assert!(config.tools.prefer_system_ffmpeg);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.toml")).unwrap();
        assert!(config.tools.yt_dlp.is_none());
    }
}
