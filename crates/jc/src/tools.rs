//! Finds the outside tools and models for pipeline commands. Unlike
//! `jc doctor`, this never downloads anything: a missing tool is a
//! `setup_incomplete` error that points to `jc doctor`.

use crate::config::Config;
use crate::doctor::models::{self, ModelSpec};
use crate::doctor::platform::Platform;
use crate::doctor::probe;
use crate::error::{ErrorKind, JcError};
use crate::paths::Paths;
use anyhow::Result;
use std::path::PathBuf;

fn missing(what: &str) -> anyhow::Error {
    JcError::new(
        ErrorKind::SetupIncomplete,
        format!("{what} is not installed"),
    )
    .with_hint("run `jc doctor`")
    .into()
}

pub fn yt_dlp(paths: &Paths, config: &Config) -> Result<PathBuf> {
    let path = config
        .tools
        .yt_dlp
        .clone()
        .unwrap_or_else(|| paths.bin_dir().join(Platform::current().exe("yt-dlp")));
    if path.is_file() {
        Ok(path)
    } else {
        Err(missing("yt-dlp"))
    }
}

/// deno, which yt-dlp uses to solve YouTube's JavaScript challenges.
/// Optional: without it yt-dlp still works, with fewer YouTube formats.
pub fn deno(paths: &Paths, config: &Config) -> Option<PathBuf> {
    let path = config
        .tools
        .deno
        .clone()
        .unwrap_or_else(|| paths.bin_dir().join(Platform::current().exe("deno")));
    path.is_file().then_some(path)
}

#[derive(Debug, Clone)]
pub struct Ffmpeg {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

/// Same order as `jc doctor`: config, then jc's own build, then a capable
/// system ffmpeg.
pub fn ffmpeg(paths: &Paths, config: &Config) -> Result<Ffmpeg> {
    let platform = Platform::current();
    if let Some(ffmpeg) = config.tools.ffmpeg.clone() {
        let ffprobe = config
            .tools
            .ffprobe
            .clone()
            .unwrap_or_else(|| ffmpeg.with_file_name(platform.exe("ffprobe")));
        return if ffmpeg.is_file() && ffprobe.is_file() {
            Ok(Ffmpeg { ffmpeg, ffprobe })
        } else {
            Err(missing("the ffmpeg set in config.toml"))
        };
    }
    let managed = Ffmpeg {
        ffmpeg: paths.bin_dir().join(platform.exe("ffmpeg")),
        ffprobe: paths.bin_dir().join(platform.exe("ffprobe")),
    };
    if managed.ffmpeg.is_file() && managed.ffprobe.is_file() {
        return Ok(managed);
    }
    if config.tools.prefer_system_ffmpeg
        && let (Some(ffmpeg), Some(ffprobe)) = (probe::which("ffmpeg"), probe::which("ffprobe"))
        && probe::ffmpeg_missing_features(&ffmpeg).is_ok_and(|m| m.is_empty())
    {
        return Ok(Ffmpeg { ffmpeg, ffprobe });
    }
    Err(missing("ffmpeg"))
}

/// The whisper model chosen in config.toml.
pub fn whisper_model(paths: &Paths, config: &Config) -> Result<(ModelSpec, PathBuf)> {
    let spec = models::whisper_model(&config.models.whisper).ok_or_else(|| {
        JcError::new(
            ErrorKind::SetupIncomplete,
            format!("unknown whisper model \"{}\"", config.models.whisper),
        )
        .with_hint(format!(
            "set models.whisper in config.toml to one of {}",
            models::whisper_ids().join(", ")
        ))
    })?;
    if !spec.is_installed(&paths.models_dir()) {
        return Err(missing(&format!("the whisper model {}", spec.id)));
    }
    Ok((spec, spec.path(&paths.models_dir())))
}

/// The voice activity detection model, required for transcription.
pub fn vad_model(paths: &Paths) -> Result<PathBuf> {
    let spec = models::SILERO_VAD;
    if spec.is_installed(&paths.models_dir()) {
        Ok(spec.path(&paths.models_dir()))
    } else {
        Err(missing("the voice activity detection model"))
    }
}
