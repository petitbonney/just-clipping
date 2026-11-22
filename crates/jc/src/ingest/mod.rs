//! `jc ingest <url|file>`: creates (or reopens) the project for a source and
//! downloads everything analysis needs: the video, its metadata, its chat
//! replay and, for YouTube, the "most replayed" heatmap (inside info.json).
//!
//! Rerunning is safe: an existing project for the same video is reused,
//! finished downloads are kept, and an interrupted yt-dlp download resumes.

pub mod source;

use crate::chat::{self, ChatMessage, ChatOrigin, twitch::TwitchGql};
use crate::cli::IngestArgs;
use crate::config::Config;
use crate::data::{Data, ProfileHints};
use crate::error::{ErrorKind, JcError};
use crate::output::{self, Report, json_of};
use crate::paths::{self, Paths};
use crate::process;
use crate::project::{Manifest, ProjectDir, Rights, Source, SourceKind, Stage, StageStatus};
use crate::status::{self, NextStep};
use crate::tools::{self, Ffmpeg};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use source::SourceRef;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const INFO_FILE: &str = "source/info.json";
pub const PROBE_FILE: &str = "source/probe.json";
pub const CHAT_FILE: &str = "source/chat.jsonl";

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ChatSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<ChatOrigin>,
    pub messages: usize,
    /// Why there is no chat, when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<String>,
}

impl ChatSummary {
    fn found(origin: ChatOrigin, messages: usize) -> Self {
        Self {
            origin: Some(origin),
            messages,
            missing: None,
        }
    }

    fn missing(reason: impl Into<String>) -> Self {
        Self {
            origin: None,
            messages: 0,
            missing: Some(reason.into()),
        }
    }
}

#[derive(Serialize)]
pub struct IngestReport {
    pub project: String,
    pub path: PathBuf,
    /// False when an existing project was reopened.
    pub created: bool,
    pub source: Source,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub chat: ChatSummary,
    /// Seconds of YouTube "most replayed" data, 0 if none.
    pub heatmap_points: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<NextStep>,
}

impl Report for IngestReport {
    fn to_json(&self) -> Value {
        json_of(self)
    }

    fn to_human(&self, _quiet: bool) -> String {
        let mut out = String::new();
        let verb = if self.created { "Created" } else { "Updated" };
        let _ = writeln!(out, "{verb} project {}", self.project);
        let _ = writeln!(out, "folder    {}", self.path.display());
        if let Some(t) = &self.source.title {
            let _ = writeln!(out, "title     {t}");
        }
        if let Some(d) = self.source.duration_s {
            let _ = writeln!(out, "duration  {}", chat::twitch::hms(d));
        }
        if let Some(v) = &self.source.video {
            let _ = writeln!(out, "video     {v}");
        }
        let chat = match (&self.chat.origin, &self.chat.missing) {
            (Some(_), _) => format!("{} messages", self.chat.messages),
            (None, Some(why)) => format!("none ({why})"),
            (None, None) => "none".into(),
        };
        let _ = writeln!(out, "chat      {chat}");
        if self.heatmap_points > 0 {
            let _ = writeln!(out, "heatmap   yes");
        }
        let _ = writeln!(
            out,
            "profile   {}   language {}",
            self.profile.as_deref().unwrap_or("? (Claude decides)"),
            self.language
                .as_deref()
                .unwrap_or("detected during analyze")
        );
        if let Some(next) = &self.next
            && let Some(cmd) = &next.command
        {
            let _ = writeln!(out, "\nnext: {cmd}");
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

pub fn run(paths: &Paths, config: &Config, args: &IngestArgs) -> Result<IngestReport> {
    let source = source::classify(&args.source)?;
    let data = Data::load(Some(&paths.workspace.join("data")))?;
    if let Some(p) = &args.profile
        && data.profile(p).is_none()
    {
        return Err(
            JcError::new(ErrorKind::Other, format!("unknown profile \"{p}\""))
                .with_hint(format!(
                    "choose one of {}",
                    data.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
                .into(),
        );
    }
    let ffmpeg = tools::ffmpeg(paths, config)?;
    let yt_dlp = match source {
        SourceRef::Local { .. } => None,
        _ => Some(tools::yt_dlp(paths, config)?),
    };
    let ytdlp = yt_dlp.map(|bin| YtDlp {
        bin,
        deno: tools::deno(paths, config),
        ffmpeg: ffmpeg.clone(),
    });

    let (dir, mut manifest, created) = match find_existing(&paths.projects_dir(), &source)? {
        Some(dir) => {
            let m = dir.load()?;
            (dir, m, false)
        }
        None => {
            let (dir, m) = create_project(paths, &data, &source, ytdlp.as_ref(), &ffmpeg, args)?;
            (dir, m, true)
        }
    };
    if let Some(p) = &args.profile {
        manifest.profile = Some(p.clone());
    }
    if let Some(l) = &args.language {
        manifest.language = Some(l.to_ascii_lowercase());
    }
    if let Some(who) = &args.clipping_for {
        manifest.source.rights.clipping_for = Some(who.clone());
    }

    let already_done = manifest.stages.ingest.status == StageStatus::Done;
    manifest.set_stage(Stage::Ingest, StageStatus::Running, None);
    dir.save(&manifest)?;

    let result = ingest_steps(
        &dir,
        &mut manifest,
        config,
        &source,
        ytdlp.as_ref(),
        &ffmpeg,
        args,
        already_done,
    );
    match result {
        Ok(chat) => {
            let info = read_json(&dir.root.join(INFO_FILE));
            let heatmap_points = info
                .as_ref()
                .and_then(|i| i.get("heatmap"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            manifest.set_stage(Stage::Ingest, StageStatus::Done, None);
            let extra = &mut manifest.stages.ingest.extra;
            extra.insert("chat".into(), json_of(&chat));
            extra.insert("heatmap".into(), (heatmap_points > 0).into());
            dir.save(&manifest)?;
            let next = status::next_step(&manifest, dir.proposed_clips_path().is_file());
            Ok(IngestReport {
                project: manifest.id.clone(),
                path: dir.root.clone(),
                created,
                profile: manifest.profile.clone(),
                language: manifest.language.clone(),
                source: manifest.source,
                chat,
                heatmap_points,
                next,
            })
        }
        Err(e) => {
            manifest.set_stage(Stage::Ingest, StageStatus::Failed, Some(format!("{e:#}")));
            dir.save(&manifest)?;
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ingest_steps(
    dir: &ProjectDir,
    manifest: &mut Manifest,
    config: &Config,
    source: &SourceRef,
    ytdlp: Option<&YtDlp>,
    ffmpeg: &Ffmpeg,
    args: &IngestArgs,
    already_done: bool,
) -> Result<ChatSummary> {
    // 1. Chat. Never fails the ingest: no chat only means one signal less.
    let chat_path = dir.root.join(CHAT_FILE);
    let chat = if let Some(file) = &args.chat {
        output::log(format!("Importing chat from {}…", file.display()));
        let (mut msgs, origin) = chat::import(file)?;
        chat::write_jsonl(&chat_path, &mut msgs)?;
        ChatSummary::found(origin, msgs.len())
    } else if chat_path.is_file() {
        previous_chat(manifest, &chat_path)?
    } else if args.no_chat {
        ChatSummary::missing("skipped with --no-chat")
    } else {
        fetch_chat(dir, manifest, source, ytdlp, &chat_path)
    };

    // 2. Video.
    if !(already_done && manifest.source.video.is_some()) {
        let video = match source {
            SourceRef::Local { path } => link_local(dir, path)?,
            _ => {
                let ytdlp = ytdlp.expect("remote sources have yt-dlp");
                download_video(dir, ytdlp, manifest, config.ingest.max_height)?
            }
        };
        manifest.source.video = Some(video);
    }

    // 3. Probe the final file.
    let video = dir
        .root
        .join(manifest.source.video.as_deref().unwrap_or_default());
    let probe = ffprobe(ffmpeg, &video)?;
    let has_video = probe
        .get("streams")
        .and_then(Value::as_array)
        .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "video"));
    if !has_video {
        anyhow::bail!("{} has no video stream", video.display());
    }
    if let Some(d) = probe
        .pointer("/format/duration")
        .and_then(Value::as_str)
        .and_then(|d| d.parse::<f64>().ok())
    {
        manifest.source.duration_s = Some(d);
    }
    paths::write_atomic(
        &dir.root.join(PROBE_FILE),
        serde_json::to_string_pretty(&probe)?.as_bytes(),
    )?;
    Ok(chat)
}

fn previous_chat(manifest: &Manifest, chat_path: &Path) -> Result<ChatSummary> {
    let recorded: Option<ChatSummary> = manifest.stages.ingest.extra.get("chat").and_then(|c| {
        let origin = serde_json::from_value(c.get("origin")?.clone()).ok()?;
        Some(ChatSummary::found(origin, 0))
    });
    let count = chat::read_jsonl(chat_path)?.len();
    Ok(match recorded {
        Some(mut s) => {
            s.messages = count;
            s
        }
        None => ChatSummary::found(ChatOrigin::Jsonl, count),
    })
}

fn fetch_chat(
    dir: &ProjectDir,
    manifest: &Manifest,
    source: &SourceRef,
    ytdlp: Option<&YtDlp>,
    chat_path: &Path,
) -> ChatSummary {
    let result: Result<Option<(Vec<ChatMessage>, ChatOrigin)>> = match source {
        SourceRef::Local { .. } => {
            return ChatSummary::missing("local file; import one with --chat <file>");
        }
        SourceRef::Twitch { video_id } => {
            output::log("Downloading Twitch chat…");
            TwitchGql::new()
                .and_then(|gql| gql.comments(video_id, manifest.source.duration_s))
                .map(|m| Some((m, ChatOrigin::TwitchGql)))
        }
        SourceRef::Youtube { .. } => {
            let has_replay = read_json(&dir.root.join(INFO_FILE))
                .and_then(|i| i.pointer("/subtitles/live_chat").cloned())
                .is_some();
            if !has_replay {
                return ChatSummary::missing("this video has no live chat replay");
            }
            output::log("Downloading YouTube live chat replay…");
            ytdlp
                .expect("remote sources have yt-dlp")
                .live_chat(dir, manifest.source.url.as_deref().unwrap_or_default())
                .map(|m| Some((m, ChatOrigin::YoutubeReplay)))
        }
    };
    match result {
        Ok(Some((mut msgs, origin))) => match chat::write_jsonl(chat_path, &mut msgs) {
            Ok(()) => ChatSummary::found(origin, msgs.len()),
            Err(e) => ChatSummary::missing(format!("{e:#}")),
        },
        Ok(None) => ChatSummary::missing("not available"),
        Err(e) => {
            output::log(format!("warning: no chat: {e:#}"));
            ChatSummary::missing(format!(
                "{e:#}. Import an export instead with `jc ingest <source> --chat <file>`"
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

/// The project already made for this source, if any.
pub fn find_existing(projects_dir: &Path, source: &SourceRef) -> Result<Option<ProjectDir>> {
    let Ok(entries) = std::fs::read_dir(projects_dir) else {
        return Ok(None);
    };
    for entry in entries.flatten() {
        let dir = ProjectDir::new(entry.path());
        if !ProjectDir::is_project(&dir.root) {
            continue;
        }
        let Ok(m) = dir.load() else { continue };
        let same = match source {
            SourceRef::Local { path } => m.source.path.as_deref() == Some(path.as_path()),
            _ => {
                m.source.video_id.is_some()
                    && m.source.video_id == source.video_id()
                    && kind_platform(m.source.kind) == source.platform()
            }
        };
        if same {
            return Ok(Some(dir));
        }
    }
    Ok(None)
}

fn kind_platform(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::YoutubeVod | SourceKind::YoutubeLive => "youtube",
        SourceKind::TwitchVod | SourceKind::TwitchLive => "twitch",
        SourceKind::Local => "local",
    }
}

fn create_project(
    paths: &Paths,
    data: &Data,
    source: &SourceRef,
    ytdlp: Option<&YtDlp>,
    ffmpeg: &Ffmpeg,
    args: &IngestArgs,
) -> Result<(ProjectDir, Manifest)> {
    let mut hints = ProfileHints::default();
    let (src, info) = match source {
        SourceRef::Local { path } => {
            let probe = ffprobe(ffmpeg, path)?;
            let title = probe
                .pointer("/format/tags/title")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()));
            hints.title = title.clone().unwrap_or_default();
            (
                Source {
                    kind: SourceKind::Local,
                    url: None,
                    path: Some(path.clone()),
                    channel: None,
                    title,
                    duration_s: None,
                    video_id: None,
                    video: None,
                    rights: Rights::default(),
                },
                None,
            )
        }
        _ => {
            let url = source.canonical_url().expect("remote source");
            output::log("Reading video metadata…");
            let info = ytdlp.expect("remote sources have yt-dlp").info(&url)?;
            if info.get("is_live").and_then(Value::as_bool) == Some(true)
                || info.get("live_status").and_then(Value::as_str) == Some("is_upcoming")
            {
                return Err(JcError::new(
                    ErrorKind::Other,
                    "this stream is live or has not started yet",
                )
                .with_hint("live streams come with live mode (phase 2); ingest the VOD once the stream has ended")
                .into());
            }
            let s = |k: &str| info.get(k).and_then(Value::as_str).map(String::from);
            let (kind, channel) = match source {
                SourceRef::Twitch { video_id } => {
                    if let Ok(v) = TwitchGql::new().and_then(|g| g.video_info(video_id)) {
                        hints.twitch_game = v.game;
                    }
                    (
                        SourceKind::TwitchVod,
                        s("uploader_id").or_else(|| s("uploader")),
                    )
                }
                _ => (
                    SourceKind::YoutubeVod,
                    s("channel").or_else(|| s("uploader")),
                ),
            };
            hints.title = s("title").unwrap_or_default();
            hints.youtube_categories = info
                .get("categories")
                .and_then(Value::as_array)
                .map(|c| {
                    c.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (
                Source {
                    kind,
                    url: Some(url),
                    path: None,
                    channel,
                    title: s("title"),
                    duration_s: info.get("duration").and_then(Value::as_f64),
                    video_id: source.video_id(),
                    video: None,
                    rights: Rights::default(),
                },
                Some(info),
            )
        }
    };

    let today = jiff::Timestamp::now()
        .to_zoned_datetime(jiff::tz::TimeZone::system())
        .date();
    let base = source::project_id(today, source, src.channel.as_deref());
    let mut id = base.clone();
    let mut n = 2;
    while paths.projects_dir().join(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let mut manifest = Manifest::new(&id, src);
    manifest.profile = args.profile.clone().or_else(|| data.detect_profile(&hints));
    manifest.language = args.language.as_ref().map(|l| l.to_ascii_lowercase());
    let dir = ProjectDir::create(paths.projects_dir().join(&id), &manifest)?;
    if let Some(info) = info {
        paths::write_atomic(
            &dir.root.join(INFO_FILE),
            serde_json::to_string(&info)?.as_bytes(),
        )?;
    }
    Ok((dir, manifest))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

fn ffprobe(ffmpeg: &Ffmpeg, video: &Path) -> Result<Value> {
    let out = process::output(
        Command::new(&ffmpeg.ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(video),
    )
    .with_context(|| format!("reading {}", video.display()))?;
    serde_json::from_slice(&out).context("ffprobe returned invalid JSON")
}

/// Puts a local file into `source/` without copying when possible.
fn link_local(dir: &ProjectDir, path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| "mp4".into());
    let rel = format!("source/video.{ext}");
    let dest = dir.root.join(&rel);
    if dest.exists() {
        return Ok(rel);
    }
    if std::fs::hard_link(path, &dest).is_ok() {
        return Ok(rel);
    }
    #[cfg(unix)]
    if std::os::unix::fs::symlink(path, &dest).is_ok() {
        return Ok(rel);
    }
    output::log(format!("Copying {} into the project…", path.display()));
    std::fs::copy(path, &dest).with_context(|| format!("copying {}", path.display()))?;
    Ok(rel)
}

pub struct YtDlp {
    bin: PathBuf,
    deno: Option<PathBuf>,
    ffmpeg: Ffmpeg,
}

impl YtDlp {
    fn command(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.arg("--ignore-config")
            .arg("--no-playlist")
            .arg("--ffmpeg-location")
            .arg(&self.ffmpeg.ffmpeg);
        if let Some(deno) = &self.deno {
            c.arg("--js-runtimes")
                .arg(format!("deno:{}", deno.display()));
        }
        c
    }

    fn info(&self, url: &str) -> Result<Value> {
        let out = process::output(self.command().arg("-J").arg(url))?;
        serde_json::from_slice(&out).context("yt-dlp returned invalid JSON")
    }

    fn live_chat(&self, dir: &ProjectDir, url: &str) -> Result<Vec<ChatMessage>> {
        let tmp = dir.root.join("source/.chat-download");
        std::fs::create_dir_all(&tmp)?;
        let result = (|| {
            process::run_logged(
                self.command()
                    .args(["--quiet", "--progress", "--newline"])
                    .args([
                        "--skip-download",
                        "--write-subs",
                        "--sub-langs",
                        "live_chat",
                    ])
                    .arg("-o")
                    .arg(tmp.join("chat"))
                    .arg(url),
            )?;
            chat::parse_youtube_file(&tmp.join("chat.live_chat.json"))
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        result
    }
}

fn download_video(
    dir: &ProjectDir,
    ytdlp: &YtDlp,
    manifest: &Manifest,
    max_height: u32,
) -> Result<String> {
    let source_dir = dir.root.join("source");
    if let Some(found) = find_video(&source_dir) {
        return Ok(found);
    }
    output::log(format!("Downloading the video (up to {max_height}p)…"));
    let url = manifest.source.url.as_deref().context("no source URL")?;
    process::run_logged(
        ytdlp
            .command()
            .args(["--quiet", "--progress", "--newline", "--continue"])
            .args(["-f", "bv*+ba/b"])
            .args(["-S", &format!("res:{max_height},vcodec:h264,acodec:m4a")])
            .args(["--merge-output-format", "mp4"])
            .arg("-o")
            .arg(source_dir.join("video.%(ext)s"))
            .arg(url),
    )
    .context("downloading the video")?;
    find_video(&source_dir).context("yt-dlp finished but no video file was written")
}

/// `source/video.<ext>`, ignoring partial downloads.
fn find_video(source_dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(source_dir).ok()?;
    let mut found: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            let parts: Vec<&str> = n.split('.').collect();
            // yt-dlp's per-format pieces look like video.f137.mp4.
            let format_piece = parts.iter().any(|p| {
                p.len() > 1 && p.starts_with('f') && p[1..].bytes().all(|b| b.is_ascii_digit())
            });
            parts.len() == 2
                && parts[0] == "video"
                && !format_piece
                && !["part", "ytdl", "temp", "tmp"].contains(&parts[1])
        })
        .collect();
    found.sort();
    found.into_iter().next().map(|n| format!("source/{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_video_skips_partials() {
        let dir = tempfile::tempdir().unwrap();
        for n in [
            "video.mp4.part",
            "video.f137.mp4",
            "video.temp.mp4",
            "info.json",
        ] {
            std::fs::write(dir.path().join(n), b"").unwrap();
        }
        assert_eq!(find_video(dir.path()), None);
        std::fs::write(dir.path().join("video.mp4"), b"").unwrap();
        assert_eq!(find_video(dir.path()).as_deref(), Some("source/video.mp4"));
        let flv = tempfile::tempdir().unwrap();
        std::fs::write(flv.path().join("video.flv"), b"").unwrap();
        assert_eq!(find_video(flv.path()).as_deref(), Some("source/video.flv"));
    }
}
