//! `jc doctor`: checks and installs the outside tools (yt-dlp, ffmpeg,
//! ffprobe) and reports which GPU backend transcription will use.
//!
//! - **yt-dlp** is always jc's own copy in the cache, because platforms change
//!   weekly and jc must be able to update it. It is refreshed when the last
//!   update check is older than `doctor.yt_dlp_max_age_days`.
//! - **ffmpeg/ffprobe** come from `config.toml` if set, else jc's own pinned
//!   build if installed, else the system ffmpeg if it has every feature jc
//!   needs (libass, x264, loudness filters), else jc downloads its own.
//! - **Models**: the whisper model chosen in `config.toml` and YuNet, pinned
//!   by hash.

pub mod download;
pub mod models;
pub mod platform;
pub mod probe;
pub mod state;

use crate::cli::DoctorArgs;
use crate::config::Config;
use crate::error::ErrorKind;
use crate::output::{Report, json_of};
use crate::paths::Paths;
use anyhow::{Context, Result};
use download::Http;
use jiff::Timestamp;
use platform::{FfmpegBuild, FfmpegSource, Platform};
use serde::Serialize;
use state::{DenoState, FfmpegState, ToolState, YtDlpState};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// Present and usable.
    Ok,
    /// Downloaded during this run.
    Installed,
    /// Replaced by a newer version during this run.
    Updated,
    /// Not installed (with `--check`, or when there is no build for this OS).
    Missing,
    /// Present but lacking a feature jc needs.
    Unsuitable,
    /// Installing or checking failed.
    Error,
}

impl ToolStatus {
    fn is_ok(self) -> bool {
        matches!(
            self,
            ToolStatus::Ok | ToolStatus::Installed | ToolStatus::Updated
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            ToolStatus::Ok => "ok",
            ToolStatus::Installed => "installed",
            ToolStatus::Updated => "updated",
            ToolStatus::Missing => "missing",
            ToolStatus::Unsuitable => "unsuitable",
            ToolStatus::Error => "error",
        }
    }
}

/// Where a tool came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Downloaded and kept up to date by jc, in the cache folder.
    Managed,
    /// Found on PATH.
    System,
    /// Set in config.toml.
    Config,
}

#[derive(Debug, Serialize)]
pub struct ToolReport {
    pub name: &'static str,
    pub status: ToolStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why it is not ok, or what to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Usable, but worth knowing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl ToolReport {
    fn new(name: &'static str, status: ToolStatus) -> Self {
        Self {
            name,
            status,
            origin: None,
            path: None,
            version: None,
            message: None,
            warnings: Vec::new(),
        }
    }

    fn found(name: &'static str, status: ToolStatus, origin: Origin, path: PathBuf) -> Self {
        Self {
            origin: Some(origin),
            path: Some(path),
            ..Self::new(name, status)
        }
    }

    fn message(mut self, msg: impl Into<String>) -> Self {
        self.message = Some(msg.into());
        self
    }

    fn version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }
}

#[derive(Debug, Serialize)]
pub struct GpuReport {
    /// `metal`, `cuda`, `vulkan` or `cpu`.
    pub backend: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Later {
    pub what: &'static str,
    pub milestone: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub platform: String,
    pub workspace: PathBuf,
    pub cache: PathBuf,
    pub tools: Vec<ToolReport>,
    pub gpu: GpuReport,
    /// Things doctor will set up once a milestone needs them.
    pub later: Vec<Later>,
}

impl Report for DoctorReport {
    fn to_json(&self) -> serde_json::Value {
        json_of(self)
    }

    fn to_human(&self, quiet: bool) -> String {
        let mut out = String::new();
        if quiet && self.ok {
            return out;
        }
        if !quiet {
            let _ = writeln!(out, "jc doctor ({})", self.platform);
            let _ = writeln!(out, "workspace  {}", self.workspace.display());
            let _ = writeln!(out, "cache      {}\n", self.cache.display());
        }
        for t in &self.tools {
            if quiet && t.status.is_ok() {
                continue;
            }
            // jc's own tools live in the cache folder shown above, so only
            // name the path of tools found elsewhere.
            let origin = match (t.origin, &t.path) {
                (Some(Origin::Managed), _) => "jc's own build".to_string(),
                (Some(Origin::System), Some(p)) => format!("system: {}", p.display()),
                (Some(Origin::Config), Some(p)) => format!("config: {}", p.display()),
                _ => String::new(),
            };
            // ffmpeg versions carry a long build suffix ("9.1-https://…").
            let version = match t.name {
                "ffmpeg" | "ffprobe" => t.version.as_deref().map(short_version),
                _ => t.version.as_deref(),
            }
            .unwrap_or("");
            let _ = writeln!(
                out,
                "  {:<10} {:<8} {:<12} {}",
                t.status.as_str(),
                t.name,
                version,
                origin
            );
            if let Some(msg) = &t.message {
                let _ = writeln!(out, "             {msg}");
            }
            for w in &t.warnings {
                let _ = writeln!(out, "             note: {w}");
            }
        }
        if !quiet {
            let _ = writeln!(
                out,
                "\n  gpu        {}: {}",
                self.gpu.backend, self.gpu.detail
            );
            let later: Vec<String> = self
                .later
                .iter()
                .map(|l| format!("{} ({})", l.what, l.milestone))
                .collect();
            let _ = writeln!(out, "  later      {}", later.join(", "));
        }
        out.push('\n');
        if self.ok {
            out.push_str("All set.\n");
        } else {
            let n = self.tools.iter().filter(|t| !t.status.is_ok()).count();
            let _ = writeln!(out, "{n} problem{} found.", if n == 1 { "" } else { "s" });
        }
        out
    }

    fn exit_code(&self) -> u8 {
        if self.ok {
            0
        } else {
            ErrorKind::SetupIncomplete.exit_code()
        }
    }
}

/// `9.1-https://www.martin-riedl.de` → `9.1`
fn short_version(v: &str) -> &str {
    v.split('-').next().unwrap_or(v)
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    config: &'a Config,
    args: &'a DoctorArgs,
    platform: Platform,
    bin_dir: PathBuf,
    http: Http,
    state: ToolState,
}

impl Ctx<'_> {
    fn managed(&self, name: &str) -> PathBuf {
        self.bin_dir.join(self.platform.exe(name))
    }

    fn log(&self, msg: impl AsRef<str>) {
        if !self.args.quiet {
            crate::output::log(msg);
        }
    }
}

pub fn run(paths: &Paths, config: &Config, args: &DoctorArgs) -> Result<DoctorReport> {
    let platform = Platform::current();
    let bin_dir = paths.bin_dir();
    let mut ctx = Ctx {
        config,
        args,
        platform,
        state: ToolState::load(&bin_dir),
        bin_dir,
        http: Http::new()?,
    };

    let yt_dlp = ensure_yt_dlp(&mut ctx);
    let (ffmpeg, ffprobe) = ensure_ffmpeg(&mut ctx);
    let deno = ensure_deno(&mut ctx);
    let models_dir = paths.models_dir();
    let whisper = match models::whisper_model(&config.models.whisper) {
        Some(spec) => ensure_model(&ctx, &models_dir, "whisper", spec),
        None => ToolReport::new("whisper", ToolStatus::Error).message(format!(
            "unknown whisper model \"{}\" in config.toml; choose one of {}",
            config.models.whisper,
            models::whisper_ids().join(", ")
        )),
    };
    let vad = ensure_model(&ctx, &models_dir, "vad", models::SILERO_VAD);
    let yunet = ensure_model(&ctx, &models_dir, "yunet", models::YUNET);
    let tools = vec![yt_dlp, deno, ffmpeg, ffprobe, whisper, vad, yunet];
    let ok = tools.iter().all(|t| t.status.is_ok());

    Ok(DoctorReport {
        ok,
        platform: format!("{} {}", platform.os, platform.arch),
        workspace: paths.workspace.clone(),
        cache: paths.cache.clone(),
        tools,
        gpu: detect_gpu(platform),
        later: vec![Later {
            what: "caption fonts",
            milestone: "M3",
        }],
    })
}

// ---------------------------------------------------------------------------
// deno (JavaScript runtime for yt-dlp's YouTube support)
// ---------------------------------------------------------------------------

fn ensure_deno(ctx: &mut Ctx) -> ToolReport {
    const NAME: &str = "deno";
    if let Some(path) = ctx.config.tools.deno.clone() {
        return if path.is_file() {
            ToolReport::found(NAME, ToolStatus::Ok, Origin::Config, path)
        } else {
            ToolReport::new(NAME, ToolStatus::Missing).message(format!(
                "tools.deno in config.toml points to {}, which does not exist",
                path.display()
            ))
        };
    }
    let path = ctx.managed("deno");
    let recorded = ctx.state.deno.as_ref().map(|s| s.version.clone());
    if path.is_file() && recorded.as_deref() == Some(platform::DENO_VERSION) {
        return ToolReport::found(NAME, ToolStatus::Ok, Origin::Managed, path).version(recorded);
    }
    let Some(file) = platform::deno_build(ctx.platform) else {
        return ToolReport::new(NAME, ToolStatus::Missing).message(format!(
            "no deno build for {} {}; install deno and set tools.deno in config.toml",
            ctx.platform.os, ctx.platform.arch
        ));
    };
    if ctx.args.check {
        return ToolReport::new(NAME, ToolStatus::Missing)
            .message("run `jc doctor` to download it (about 40 MB); yt-dlp needs it for YouTube");
    }
    ctx.log(format!("Downloading deno {}…", platform::DENO_VERSION));
    let result = (|| -> Result<()> {
        let tmp = ctx.bin_dir.join(".download");
        std::fs::create_dir_all(&tmp)?;
        let zip = tmp.join("deno.zip");
        let r = ctx
            .http
            .download(file.url, &zip, file.sha256, "deno")
            .and_then(|()| download::extract_from_zip(&zip, &ctx.platform.exe("deno"), &path));
        let _ = std::fs::remove_dir_all(&tmp);
        r?;
        ctx.state.deno = Some(DenoState {
            version: platform::DENO_VERSION.to_string(),
            installed_at: Timestamp::now(),
        });
        ctx.state.save(&ctx.bin_dir)
    })();
    match result {
        Ok(()) => {
            let status = if recorded.is_some() {
                ToolStatus::Updated
            } else {
                ToolStatus::Installed
            };
            ToolReport::found(NAME, status, Origin::Managed, path)
                .version(Some(platform::DENO_VERSION.to_string()))
        }
        Err(e) => ToolReport::new(NAME, ToolStatus::Error).message(format!("{e:#}")),
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

fn ensure_model(
    ctx: &Ctx,
    models_dir: &Path,
    name: &'static str,
    spec: models::ModelSpec,
) -> ToolReport {
    let path = spec.path(models_dir);
    if spec.is_installed(models_dir) {
        return ToolReport::found(name, ToolStatus::Ok, Origin::Managed, path)
            .version(Some(spec.id.to_string()));
    }
    if ctx.args.check {
        return ToolReport::new(name, ToolStatus::Missing)
            .version(Some(spec.id.to_string()))
            .message(format!(
                "run `jc doctor` to download it (about {} MB)",
                spec.approx_mb()
            ));
    }
    ctx.log(format!(
        "Downloading the {name} model {} (about {} MB)…",
        spec.id,
        spec.approx_mb()
    ));
    match ctx.http.download(spec.url, &path, spec.sha256, spec.id) {
        Ok(()) => ToolReport::found(name, ToolStatus::Installed, Origin::Managed, path)
            .version(Some(spec.id.to_string())),
        Err(e) => ToolReport::new(name, ToolStatus::Error).message(format!("{e:#}")),
    }
}

// ---------------------------------------------------------------------------
// yt-dlp
// ---------------------------------------------------------------------------

fn ensure_yt_dlp(ctx: &mut Ctx) -> ToolReport {
    const NAME: &str = "yt-dlp";
    let max_age = i64::from(ctx.config.doctor.yt_dlp_max_age_days);
    let today = Timestamp::now()
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .date();

    if let Some(path) = ctx.config.tools.yt_dlp.clone() {
        if !path.is_file() {
            return ToolReport::new(NAME, ToolStatus::Missing).message(format!(
                "tools.yt_dlp in config.toml points to {}, which does not exist",
                path.display()
            ));
        }
        let version = probe::yt_dlp_version(&path);
        let mut report =
            ToolReport::found(NAME, ToolStatus::Ok, Origin::Config, path).version(version.clone());
        if let Some(age) = version.and_then(|v| probe::yt_dlp_version_age_days(&v, today))
            && age > max_age
        {
            report.warnings.push(format!(
                "this yt-dlp is {age} days old and set in config.toml, so jc cannot update it"
            ));
        }
        return report;
    }

    let Some(asset) = platform::yt_dlp_asset(ctx.platform) else {
        return ToolReport::new(NAME, ToolStatus::Missing).message(format!(
            "no yt-dlp build for {} {}; install it and set tools.yt_dlp in config.toml",
            ctx.platform.os, ctx.platform.arch
        ));
    };
    let path = ctx.managed("yt-dlp");

    if path.is_file() {
        let recorded = ctx.state.yt_dlp.clone();
        let days_since_check = recorded
            .as_ref()
            .map(|s| Timestamp::now().duration_since(s.checked_at).as_hours() / 24);
        let fresh = days_since_check.is_some_and(|d| d < max_age);
        let version = recorded
            .as_ref()
            .map(|s| s.version.clone())
            .or_else(|| probe::yt_dlp_version(&path));
        let current =
            ToolReport::found(NAME, ToolStatus::Ok, Origin::Managed, path.clone()).version(version);

        if fresh && !ctx.args.update {
            return current;
        }
        if ctx.args.check {
            let mut current = current;
            current.warnings.push(match days_since_check {
                Some(d) => {
                    format!("last checked for updates {d} days ago; run `jc doctor` to update")
                }
                None => "never checked for updates; run `jc doctor` to update".to_string(),
            });
            return current;
        }
        ctx.log("Checking for a new yt-dlp release…");
        return match update_yt_dlp(ctx, asset, &path, recorded.as_ref()) {
            Ok((status, version)) => {
                ToolReport::found(NAME, status, Origin::Managed, path).version(Some(version))
            }
            Err(e) => {
                let mut current = current;
                current
                    .warnings
                    .push(format!("could not check for updates: {e:#}"));
                current
            }
        };
    }

    if ctx.args.check {
        return ToolReport::new(NAME, ToolStatus::Missing)
            .message("run `jc doctor` to download it (about 35 MB)");
    }
    ctx.log("Downloading yt-dlp…");
    match update_yt_dlp(ctx, asset, &path, None) {
        Ok((_, version)) => ToolReport::found(NAME, ToolStatus::Installed, Origin::Managed, path)
            .version(Some(version)),
        Err(e) => ToolReport::new(NAME, ToolStatus::Error).message(format!("{e:#}")),
    }
}

/// Installs the latest yt-dlp unless the recorded one already is the latest.
fn update_yt_dlp(
    ctx: &mut Ctx,
    asset: &str,
    path: &Path,
    recorded: Option<&YtDlpState>,
) -> Result<(ToolStatus, String)> {
    let latest = ctx
        .http
        .redirect_target(&format!("{}/releases/latest", platform::YT_DLP_REPO))?;
    let tag = latest
        .rsplit('/')
        .next()
        .filter(|t| !t.is_empty())
        .with_context(|| format!("unexpected release URL {latest}"))?
        .to_string();
    let base = format!("{}/releases/download/{tag}", platform::YT_DLP_REPO);
    let sums = ctx.http.get_text(&format!("{base}/SHA2-256SUMS"))?;
    let sha256 = download::parse_sha256sums(&sums, asset)
        .with_context(|| format!("{asset} is not listed in the {tag} checksums"))?;
    let now = Timestamp::now();

    let up_to_date = recorded.is_some_and(|r| r.sha256 == sha256)
        || (recorded.is_none() && download::sha256_file(path).is_ok_and(|h| h == sha256));
    let status = if up_to_date {
        ToolStatus::Ok
    } else {
        let existed = path.is_file();
        let new = download::with_suffix(path, ".new");
        ctx.http
            .download(&format!("{base}/{asset}"), &new, &sha256, "yt-dlp")?;
        download::make_executable(&new)?;
        std::fs::rename(&new, path).with_context(|| format!("installing {}", path.display()))?;
        if existed {
            ToolStatus::Updated
        } else {
            ToolStatus::Installed
        }
    };

    ctx.state.yt_dlp = Some(YtDlpState {
        version: tag.clone(),
        sha256,
        installed_at: match (up_to_date, recorded) {
            (true, Some(r)) => r.installed_at,
            _ => now,
        },
        checked_at: now,
    });
    ctx.state.save(&ctx.bin_dir)?;
    Ok((status, tag))
}

// ---------------------------------------------------------------------------
// ffmpeg + ffprobe
// ---------------------------------------------------------------------------

fn ensure_ffmpeg(ctx: &mut Ctx) -> (ToolReport, ToolReport) {
    // 1. Explicit paths in config.toml: use them as they are.
    if let Some(ffmpeg) = ctx.config.tools.ffmpeg.clone() {
        let ffprobe = ctx
            .config
            .tools
            .ffprobe
            .clone()
            .unwrap_or_else(|| ffmpeg.with_file_name(ctx.platform.exe("ffprobe")));
        return check_pair(ffmpeg, ffprobe, Origin::Config);
    }

    let build = platform::ffmpeg_build(ctx.platform);
    let managed = (ctx.managed("ffmpeg"), ctx.managed("ffprobe"));
    let managed_present = managed.0.is_file() && managed.1.is_file();
    let recorded_build = ctx.state.ffmpeg.as_ref().map(|s| s.build.clone());

    // 2. jc's own build, if it is the one this jc pins.
    if managed_present && build.is_some_and(|b| recorded_build.as_deref() == Some(b.id)) {
        return check_pair(managed.0, managed.1, Origin::Managed);
    }

    // 3. A system ffmpeg with every feature jc needs.
    let mut system_note = None;
    if ctx.config.tools.prefer_system_ffmpeg
        && let (Some(ffmpeg), Some(ffprobe)) = (probe::which("ffmpeg"), probe::which("ffprobe"))
    {
        match probe::ffmpeg_missing_features(&ffmpeg) {
            Ok(missing) if missing.is_empty() => {
                return check_pair(ffmpeg, ffprobe, Origin::System);
            }
            Ok(missing) => {
                system_note = Some(format!(
                    "the ffmpeg on PATH ({}) lacks {}, so jc uses its own build",
                    ffmpeg.display(),
                    missing.join(", ")
                ));
            }
            Err(e) => system_note = Some(format!("the ffmpeg on PATH failed to run: {e:#}")),
        }
    }

    let with_note = |(mut a, b): (ToolReport, ToolReport)| {
        if let Some(note) = &system_note {
            a.warnings.push(note.clone());
        }
        (a, b)
    };

    // 4. Download jc's own build.
    let Some(build) = build else {
        let msg = format!(
            "no prebuilt ffmpeg for {} {}; install one with libass and libx264, then set tools.ffmpeg in config.toml",
            ctx.platform.os, ctx.platform.arch
        );
        return with_note((
            ToolReport::new("ffmpeg", ToolStatus::Missing).message(msg),
            ToolReport::new("ffprobe", ToolStatus::Missing),
        ));
    };
    if ctx.args.check {
        let msg = if managed_present {
            format!(
                "jc's ffmpeg is outdated; run `jc doctor` to install ffmpeg {}",
                build.version
            )
        } else {
            format!(
                "run `jc doctor` to download ffmpeg {} (about {} MB)",
                build.version, build.approx_download_mb
            )
        };
        return with_note((
            ToolReport::new("ffmpeg", ToolStatus::Missing).message(msg),
            ToolReport::new("ffprobe", ToolStatus::Missing),
        ));
    }

    ctx.log(format!(
        "Downloading ffmpeg {} (about {} MB)…",
        build.version, build.approx_download_mb
    ));
    match install_ffmpeg(ctx, &build, &managed.0, &managed.1) {
        Ok(()) => {
            let status = if recorded_build.is_some() {
                ToolStatus::Updated
            } else {
                ToolStatus::Installed
            };
            let (mut a, mut b) = check_pair(managed.0, managed.1, Origin::Managed);
            for r in [&mut a, &mut b] {
                if r.status == ToolStatus::Ok {
                    r.status = status;
                }
            }
            with_note((a, b))
        }
        Err(e) => with_note((
            ToolReport::new("ffmpeg", ToolStatus::Error).message(format!("{e:#}")),
            ToolReport::new("ffprobe", ToolStatus::Error),
        )),
    }
}

/// Reports on an ffmpeg/ffprobe pair, checking ffmpeg's features.
fn check_pair(ffmpeg: PathBuf, ffprobe: PathBuf, origin: Origin) -> (ToolReport, ToolReport) {
    let ffmpeg_report = if !ffmpeg.is_file() {
        ToolReport::new("ffmpeg", ToolStatus::Missing)
            .message(format!("{} does not exist", ffmpeg.display()))
    } else {
        let version = probe::ffmpeg_version(&ffmpeg);
        match probe::ffmpeg_missing_features(&ffmpeg) {
            Ok(missing) if missing.is_empty() => {
                ToolReport::found("ffmpeg", ToolStatus::Ok, origin, ffmpeg).version(version)
            }
            Ok(missing) => ToolReport::found("ffmpeg", ToolStatus::Unsuitable, origin, ffmpeg)
                .version(version)
                .message(format!("missing {}", missing.join(", "))),
            Err(e) => ToolReport::found("ffmpeg", ToolStatus::Error, origin, ffmpeg)
                .message(format!("{e:#}")),
        }
    };
    let ffprobe_report = if !ffprobe.is_file() {
        ToolReport::new("ffprobe", ToolStatus::Missing)
            .message(format!("{} does not exist", ffprobe.display()))
    } else {
        match probe::ffmpeg_version(&ffprobe) {
            Some(v) => {
                ToolReport::found("ffprobe", ToolStatus::Ok, origin, ffprobe).version(Some(v))
            }
            None => ToolReport::found("ffprobe", ToolStatus::Error, origin, ffprobe)
                .message("failed to run"),
        }
    };
    (ffmpeg_report, ffprobe_report)
}

fn install_ffmpeg(ctx: &mut Ctx, build: &FfmpegBuild, ffmpeg: &Path, ffprobe: &Path) -> Result<()> {
    let tmp = ctx.bin_dir.join(".download");
    std::fs::create_dir_all(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let result = (|| -> Result<()> {
        let ffmpeg_name = ctx.platform.exe("ffmpeg");
        let ffprobe_name = ctx.platform.exe("ffprobe");
        match build.source {
            FfmpegSource::Pinned {
                ffmpeg: ffmpeg_file,
                ffprobe: ffprobe_file,
            } => {
                for (file, name, dest) in [
                    (ffmpeg_file, &ffmpeg_name, ffmpeg),
                    (ffprobe_file, &ffprobe_name, ffprobe),
                ] {
                    let zip = tmp.join(format!("{name}.zip"));
                    ctx.http.download(file.url, &zip, file.sha256, name)?;
                    download::extract_from_zip(&zip, name, dest)?;
                }
            }
            FfmpegSource::Channel {
                archive_url,
                checksums_url,
                archive_name,
            } => {
                let sums = ctx.http.get_text(checksums_url)?;
                let sha256 = download::parse_sha256sums(&sums, archive_name)
                    .with_context(|| format!("{archive_name} is not listed in {checksums_url}"))?;
                let zip = tmp.join(archive_name);
                ctx.http.download(archive_url, &zip, &sha256, "ffmpeg")?;
                download::extract_from_zip(&zip, &ffmpeg_name, ffmpeg)?;
                download::extract_from_zip(&zip, &ffprobe_name, ffprobe)?;
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result?;

    ctx.state.ffmpeg = Some(FfmpegState {
        build: build.id.to_string(),
        installed_at: Timestamp::now(),
    });
    ctx.state.save(&ctx.bin_dir)
}

// ---------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------

/// The best transcription backend this machine supports. whisper.cpp picks
/// its backend at build time, so this is what release builds will use.
fn detect_gpu(p: Platform) -> GpuReport {
    if p.os == "macos" {
        return if p.arch == "aarch64" {
            GpuReport {
                backend: "metal",
                detail: "Apple Silicon".to_string(),
            }
        } else {
            GpuReport {
                backend: "cpu",
                detail: "Intel Mac: whisper.cpp runs on the CPU (with Accelerate)".to_string(),
            }
        };
    }
    if probe::runs("nvidia-smi", &["-L"]) {
        return GpuReport {
            backend: "cuda",
            detail: "NVIDIA GPU found (nvidia-smi)".to_string(),
        };
    }
    if probe::runs("vulkaninfo", &["--summary"]) {
        return GpuReport {
            backend: "vulkan",
            detail: "Vulkan driver found (vulkaninfo)".to_string(),
        };
    }
    GpuReport {
        backend: "cpu",
        detail: "no GPU backend found; transcription will run on the CPU".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_version_drops_build_suffix() {
        assert_eq!(short_version("9.1-https://www.martin-riedl.de"), "9.1");
        assert_eq!(short_version("n9.0-12-gabc"), "n9.0");
        assert_eq!(short_version("2026.10.26"), "2026.10.26");
    }
}
