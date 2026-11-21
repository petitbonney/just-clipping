//! Where jc keeps things.
//!
//! - **Workspace** (projects, library, `config.toml`): `--workspace`, then
//!   `$JC_WORKSPACE`, then `~/JustClipping`.
//! - **Cache** (downloaded tools and models): `$JC_CACHE_DIR`, then the OS
//!   cache folder (`~/Library/Caches/just-clipping` on macOS,
//!   `~/.cache/just-clipping` on Linux, `%LOCALAPPDATA%\just-clipping\cache`
//!   on Windows).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const WORKSPACE_ENV: &str = "JC_WORKSPACE";
pub const CACHE_ENV: &str = "JC_CACHE_DIR";
const DEFAULT_WORKSPACE_DIR: &str = "JustClipping";
const APP_DIR: &str = "just-clipping";

#[derive(Debug, Clone)]
pub struct Paths {
    pub workspace: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    /// `workspace` is the value of `--workspace` / `$JC_WORKSPACE` (clap
    /// reads the env var).
    pub fn resolve(workspace: Option<PathBuf>) -> Result<Self> {
        let workspace = match workspace {
            Some(dir) => dir,
            None => directories::UserDirs::new()
                .context("could not find your home folder; set --workspace or JC_WORKSPACE")?
                .home_dir()
                .join(DEFAULT_WORKSPACE_DIR),
        };
        let cache = match std::env::var_os(CACHE_ENV) {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => directories::ProjectDirs::from_path(PathBuf::from(APP_DIR))
                .context("could not find the cache folder; set JC_CACHE_DIR")?
                .cache_dir()
                .to_path_buf(),
        };
        Ok(Self { workspace, cache })
    }

    pub fn config_file(&self) -> PathBuf {
        self.workspace.join("config.toml")
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.workspace.join("projects")
    }

    /// Managed tool binaries (yt-dlp, ffmpeg, ffprobe).
    pub fn bin_dir(&self) -> PathBuf {
        self.cache.join("bin")
    }

    /// Downloaded models (whisper, face detection), from M1.
    pub fn models_dir(&self) -> PathBuf {
        self.cache.join("models")
    }
}

/// Writes a file atomically: write a sibling temp file, then rename over the
/// target, so a crash never leaves a half-written manifest behind.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent folder", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}
