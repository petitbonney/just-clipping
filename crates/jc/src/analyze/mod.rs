//! `jc analyze`: turns an ingested project into per-second signals and
//! merges them into `analysis/timeline.json`, which `jc candidates` reads.
//!
//! | Step       | Output                    | From                         |
//! |------------|---------------------------|------------------------------|
//! | heatmap    | `heatmap.json`            | YouTube "most replayed"      |
//! | audio      | `audio.json`              | ffmpeg EBU R128 loudness     |
//! | scenes     | `scenes.json`             | ffmpeg scene detection       |
//! | transcript | `transcript.json`         | whisper.cpp                  |
//! | chat       | `chat_signal.json`        | `source/chat.jsonl`          |
//! | timeline   | `timeline.json`           | all of the above, 0..1 each  |
//!
//! Each step is skipped when its output exists (`--force` redoes them all),
//! and the transcript resumes after an interruption.

pub mod chat_signal;
pub mod media;
pub mod signal;
pub mod transcribe;

use crate::chat;
use crate::cli::AnalyzeArgs;
use crate::config::Config;
use crate::data::Data;
use crate::error::{ErrorKind, JcError};
use crate::ingest;
use crate::output::{self, Report, json_of};
use crate::paths::{self, Paths};
use crate::project::{self, Manifest, ProjectDir, SourceKind, Stage, StageStatus};
use crate::status::{self, NextStep};
use crate::tools::{self, Ffmpeg};
use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const TIMELINE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Timeline {
    pub version: u32,
    pub duration_s: f64,
    /// Seconds per value.
    pub step_s: f64,
    /// 0..1 per second for each signal present. Missing signals (no chat,
    /// no heatmap) are left out rather than filled with zeros.
    pub signals: BTreeMap<String, Vec<f64>>,
}

#[derive(Debug, Serialize)]
pub struct StepReport {
    pub step: &'static str,
    /// `done`, `skipped` (output already there), or `unavailable`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Serialize)]
pub struct AnalyzeReport {
    pub project: String,
    pub path: PathBuf,
    pub language: String,
    pub signals: Vec<String>,
    pub steps: Vec<StepReport>,
    pub words: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<NextStep>,
}

impl Report for AnalyzeReport {
    fn to_json(&self) -> serde_json::Value {
        json_of(self)
    }

    fn to_human(&self, _quiet: bool) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "Analyzed {}\n", self.project);
        for s in &self.steps {
            let time = s.seconds.map(|t| format!("{t:.0}s")).unwrap_or_default();
            let _ = writeln!(
                out,
                "  {:<11} {:<11} {:>6}  {}",
                s.step,
                s.status,
                time,
                s.note.as_deref().unwrap_or("")
            );
        }
        let _ = writeln!(
            out,
            "\nlanguage  {}\nwords     {}\nsignals   {}",
            self.language,
            self.words,
            self.signals.join(", ")
        );
        if let Some(next) = &self.next
            && let Some(cmd) = &next.command
        {
            let _ = writeln!(out, "\nnext: {cmd}");
        }
        out
    }
}

struct Ctx<'a> {
    dir: &'a ProjectDir,
    analysis: PathBuf,
    ffmpeg: Ffmpeg,
    video: PathBuf,
    duration_s: f64,
    force: bool,
    steps: Vec<StepReport>,
}

impl Ctx<'_> {
    fn path(&self, file: &str) -> PathBuf {
        self.analysis.join(file)
    }

    /// Runs `make` unless `file` exists, then returns the file's content.
    fn step<T: Serialize + DeserializeOwned>(
        &mut self,
        name: &'static str,
        file: &str,
        make: impl FnOnce(&Self) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        let path = self.path(file);
        if !self.force
            && let Some(v) = read_json::<T>(&path)
        {
            self.steps.push(StepReport {
                step: name,
                status: "skipped",
                seconds: None,
                note: Some("already done".into()),
            });
            return Ok(Some(v));
        }
        output::log(format!("Analyzing {name}…"));
        let started = Instant::now();
        let value = make(self).with_context(|| format!("analyzing {name}"))?;
        let secs = started.elapsed().as_secs_f64();
        match &value {
            Some(v) => {
                paths::write_atomic(&path, serde_json::to_string(v)?.as_bytes())?;
                self.steps.push(StepReport {
                    step: name,
                    status: "done",
                    seconds: Some(secs),
                    note: None,
                });
            }
            None => {
                let _ = std::fs::remove_file(&path);
                self.steps.push(StepReport {
                    step: name,
                    status: "unavailable",
                    seconds: None,
                    note: None,
                });
            }
        }
        Ok(value)
    }

    fn note(&mut self, note: impl Into<String>) {
        if let Some(s) = self.steps.last_mut() {
            s.note = Some(note.into());
        }
    }
}

struct Models<'a> {
    whisper: &'a Path,
    whisper_id: &'a str,
    vad: &'a Path,
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub fn run(paths: &Paths, config: &Config, args: &AnalyzeArgs) -> Result<AnalyzeReport> {
    let dir = project::require(&paths.projects_dir(), args.project.project.as_deref())?;
    let mut manifest = dir.load()?;
    if manifest.stages.ingest.status != StageStatus::Done {
        return Err(
            JcError::new(ErrorKind::Other, "this project is not ingested yet")
                .with_hint(format!(
                    "run `jc ingest` first (`jc status -p {}`)",
                    manifest.id
                ))
                .into(),
        );
    }
    if let Some(l) = &args.language {
        manifest.language = Some(l.to_ascii_lowercase());
    }
    let ffmpeg = tools::ffmpeg(paths, config)?;
    let (model, model_path) = tools::whisper_model(paths, config)?;
    let vad_path = tools::vad_model(paths)?;
    let data = Data::load(Some(&paths.workspace.join("data")))?;

    manifest.set_stage(Stage::Analyze, StageStatus::Running, None);
    dir.save(&manifest)?;
    let result = analyze(
        &dir,
        &mut manifest,
        config,
        &data,
        ffmpeg,
        Models {
            whisper: &model_path,
            whisper_id: model.id,
            vad: &vad_path,
        },
        args.force,
    );
    match result {
        Ok(report) => Ok(report),
        Err(e) => {
            manifest.set_stage(Stage::Analyze, StageStatus::Failed, Some(format!("{e:#}")));
            dir.save(&manifest)?;
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze(
    dir: &ProjectDir,
    manifest: &mut Manifest,
    config: &Config,
    data: &Data,
    ffmpeg: Ffmpeg,
    models: Models,
    force: bool,
) -> Result<AnalyzeReport> {
    let video = dir.root.join(
        manifest
            .source
            .video
            .as_deref()
            .context("the project has no video")?,
    );
    let duration_s = manifest
        .source
        .duration_s
        .context("the project has no duration; rerun `jc ingest`")?;
    let analysis = dir.root.join("analysis");
    std::fs::create_dir_all(&analysis)?;
    let mut ctx = Ctx {
        dir,
        analysis,
        ffmpeg,
        video,
        duration_s,
        force,
        steps: Vec::new(),
    };

    // Cheap steps first.
    let heatmap = ctx.step("heatmap", "heatmap.json", |c| {
        let info: Option<serde_json::Value> = read_json(&c.dir.root.join(ingest::INFO_FILE));
        Ok(info.and_then(|i| media::heatmap(&i, c.duration_s)))
    })?;
    let audio = ctx.step("audio", "audio.json", |c| {
        media::audio(&c.ffmpeg, &c.video, c.duration_s).map(Some)
    })?;
    let scenes = ctx.step("scenes", "scenes.json", |c| {
        media::scenes(&c.ffmpeg, &c.video, c.duration_s).map(Some)
    })?;
    if let Some(s) = &scenes {
        let n = s.cuts.len();
        ctx.note(format!("{n} cuts"));
    }

    // Transcript (the long one).
    let language = manifest.language.clone();
    let transcript = ctx
        .step("transcript", "transcript.json", |c| {
            let opts = transcribe::Options {
                model_path: models.whisper,
                vad_model_path: models.vad,
                model_id: models.whisper_id,
                language: language.clone(),
                threads: config.transcribe.threads,
                beam_size: config.transcribe.beam_size,
            };
            let partial = transcribe::partial_path(&c.analysis);
            let t = transcribe::transcribe(&c.ffmpeg, &c.video, c.duration_s, &opts, &partial)?;
            let _ = std::fs::remove_file(&partial);
            Ok(Some(t))
        })?
        .expect("transcript always produces output");
    let words: usize = transcript.segments.iter().map(|s| s.words.len()).sum();
    ctx.note(format!("{words} words, {}", transcript.language));
    // Only trust a detected language when there was speech to detect it on.
    if manifest.language.is_none() && words > 0 {
        manifest.language = Some(transcript.language.clone());
    }
    let language = manifest.language.clone().unwrap_or_default();

    // Chat, with the hype words of the spoken language plus English (chat
    // slang like "lol" and "gg" is everywhere).
    let chat_path = dir.root.join(ingest::CHAT_FILE);
    let delay = match manifest.source.kind {
        SourceKind::TwitchVod | SourceKind::TwitchLive => config.chat.twitch_delay_s,
        SourceKind::YoutubeVod | SourceKind::YoutubeLive => config.chat.youtube_delay_s,
        SourceKind::Local => config.chat.twitch_delay_s,
    };
    let matcher = hype_matcher(data, &language, manifest.profile.as_deref());
    let chat_sig = ctx.step("chat", "chat_signal.json", |c| {
        if !chat_path.is_file() {
            return Ok(None);
        }
        let msgs = chat::read_jsonl(&chat_path)?;
        if msgs.is_empty() {
            return Ok(None);
        }
        Ok(Some(chat_signal::compute(
            &msgs,
            c.duration_s,
            delay,
            &matcher,
        )))
    })?;
    if let Some(c) = &chat_sig {
        let n = c.messages;
        ctx.note(format!("{n} messages, shifted {delay}s"));
    }

    // Merge. Always rebuilt: it is cheap and must match its inputs.
    let n = signal::bins(duration_s);
    let mut signals = BTreeMap::new();
    let fit = |v: &[f64]| -> Vec<f64> {
        let mut v = v.to_vec();
        v.resize(n, 0.0);
        v
    };
    signals.insert("speech".to_string(), speech_density(&transcript, n));
    if let Some(a) = &audio {
        signals.insert("audio".into(), fit(&a.score));
    }
    if let Some(s) = &scenes {
        signals.insert("scenes".into(), fit(&s.score));
    }
    if let Some(h) = &heatmap {
        signals.insert("heatmap".into(), fit(&h.values));
    }
    if let Some(c) = &chat_sig {
        signals.insert("chat_rate".into(), fit(&c.rate_score));
        signals.insert("chat_hype".into(), fit(&c.hype_score));
    }
    let timeline = Timeline {
        version: TIMELINE_VERSION,
        duration_s,
        step_s: 1.0,
        signals,
    };
    paths::write_atomic(
        &ctx.path("timeline.json"),
        serde_json::to_string(&timeline)?.as_bytes(),
    )?;
    let names: Vec<String> = timeline.signals.keys().cloned().collect();

    manifest.set_stage(Stage::Analyze, StageStatus::Done, None);
    manifest
        .stages
        .analyze
        .extra
        .insert("signals".into(), json_of(&names));
    dir.save(manifest)?;

    Ok(AnalyzeReport {
        project: manifest.id.clone(),
        path: dir.root.clone(),
        language,
        signals: names,
        steps: ctx.steps,
        words,
        next: status::next_step(manifest, dir.proposed_clips_path().is_file()),
    })
}

fn hype_matcher(data: &Data, language: &str, profile: Option<&str>) -> chat_signal::HypeMatcher {
    let mut lexicon: Vec<&String> = Vec::new();
    for code in [language, "en"] {
        if let Some(l) = data.lang(code) {
            lexicon.extend(&l.chat_hype);
        }
    }
    let emotes: Vec<&String> = match profile.and_then(|p| data.profile(p)) {
        Some(p) => p.chat_emotes.iter().collect(),
        None => data
            .profiles
            .values()
            .flat_map(|p| &p.chat_emotes)
            .collect(),
    };
    chat_signal::HypeMatcher::new(lexicon, emotes)
}

/// 0..1 per second: words spoken in that second (4 words/s = 1).
fn speech_density(t: &transcribe::Transcript, n: usize) -> Vec<f64> {
    let mut counts = vec![0.0; n];
    for w in t.segments.iter().flat_map(|s| &s.words) {
        let mid = (w.t0 + w.t1) / 2.0;
        let i = mid as usize;
        if i < n {
            counts[i] += 1.0;
        }
    }
    signal::round(
        &counts
            .iter()
            .map(|c| signal::clamp01(c / 4.0))
            .collect::<Vec<_>>(),
        3,
    )
}
