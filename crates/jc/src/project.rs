//! Projects: the folder layout and the `project.json` manifest, which is the
//! source of truth for where a project is in the pipeline.

use crate::error::{ErrorKind, JcError};
use crate::paths;
use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Version of the `project.json` format this build writes.
pub const SCHEMA: u32 = 1;
pub const MANIFEST_FILE: &str = "project.json";

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub created_at: Timestamp,
    pub source: Source,
    /// Detected during analysis, overridable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Content profile (gaming, podcast, irl), detected, overridable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Caption style preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default)]
    pub stages: Stages,
    /// Clip ids, in the order they were proposed.
    #[serde(default)]
    pub clips: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Original path, for local files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(default)]
    pub rights: Rights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    YoutubeVod,
    YoutubeLive,
    TwitchVod,
    TwitchLive,
    Local,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::YoutubeVod => "youtube_vod",
            SourceKind::YoutubeLive => "youtube_live",
            SourceKind::TwitchVod => "twitch_vod",
            SourceKind::TwitchLive => "twitch_live",
            SourceKind::Local => "local",
        }
    }
}

/// Who the user is clipping for, recorded per project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Rights {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipping_for: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

/// Pipeline stages, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Ingest,
    Analyze,
    Candidates,
    ReviewClips,
    Render,
    Package,
    ReviewPackage,
    Export,
}

impl Stage {
    pub const ALL: [Stage; 8] = [
        Stage::Ingest,
        Stage::Analyze,
        Stage::Candidates,
        Stage::ReviewClips,
        Stage::Render,
        Stage::Package,
        Stage::ReviewPackage,
        Stage::Export,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Ingest => "ingest",
            Stage::Analyze => "analyze",
            Stage::Candidates => "candidates",
            Stage::ReviewClips => "review_clips",
            Stage::Render => "render",
            Stage::Package => "package",
            Stage::ReviewPackage => "review_package",
            Stage::Export => "export",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
}

impl StageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            StageStatus::Pending => "pending",
            StageStatus::Running => "running",
            StageStatus::Done => "done",
            StageStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageRecord {
    pub status: StageStatus,
    /// When the stage last changed status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Stage-specific details (for example `signals` for analyze), kept as
    /// they are so older builds do not drop fields written by newer ones.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// One record per stage. Serialized as an object keyed by stage name, in
/// pipeline order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stages {
    pub ingest: StageRecord,
    pub analyze: StageRecord,
    pub candidates: StageRecord,
    pub review_clips: StageRecord,
    pub render: StageRecord,
    pub package: StageRecord,
    pub review_package: StageRecord,
    pub export: StageRecord,
}

impl Stages {
    pub fn get(&self, stage: Stage) -> &StageRecord {
        match stage {
            Stage::Ingest => &self.ingest,
            Stage::Analyze => &self.analyze,
            Stage::Candidates => &self.candidates,
            Stage::ReviewClips => &self.review_clips,
            Stage::Render => &self.render,
            Stage::Package => &self.package,
            Stage::ReviewPackage => &self.review_package,
            Stage::Export => &self.export,
        }
    }

    pub fn get_mut(&mut self, stage: Stage) -> &mut StageRecord {
        match stage {
            Stage::Ingest => &mut self.ingest,
            Stage::Analyze => &mut self.analyze,
            Stage::Candidates => &mut self.candidates,
            Stage::ReviewClips => &mut self.review_clips,
            Stage::Render => &mut self.render,
            Stage::Package => &mut self.package,
            Stage::ReviewPackage => &mut self.review_package,
            Stage::Export => &mut self.export,
        }
    }

    /// The first stage that is not done, or `None` when the project is
    /// finished.
    pub fn first_unfinished(&self) -> Option<Stage> {
        Stage::ALL
            .into_iter()
            .find(|&s| self.get(s).status != StageStatus::Done)
    }
}

impl Manifest {
    pub fn new(id: impl Into<String>, source: Source) -> Self {
        Self {
            schema: SCHEMA,
            id: id.into(),
            created_at: Timestamp::now(),
            source,
            language: None,
            profile: None,
            style: None,
            stages: Stages::default(),
            clips: Vec::new(),
        }
    }

    /// Records a stage's new status with the current time.
    pub fn set_stage(&mut self, stage: Stage, status: StageStatus, error: Option<String>) {
        let record = self.stages.get_mut(stage);
        record.status = status;
        record.at = Some(Timestamp::now());
        record.error = error;
    }

    /// Parses a manifest, upgrading older schema versions first.
    pub fn from_json(text: &str) -> Result<Self> {
        let mut value: serde_json::Value = serde_json::from_str(text).context("invalid JSON")?;
        let schema = value
            .get("schema")
            .and_then(|s| s.as_u64())
            .context("missing \"schema\" version")?;
        let schema = u32::try_from(schema).context("invalid \"schema\" version")?;
        if schema > SCHEMA {
            return Err(JcError::new(
                ErrorKind::Other,
                format!(
                    "this project uses manifest schema {schema}, but this jc only knows up to {SCHEMA}"
                ),
            )
            .with_hint("update jc to open this project")
            .into());
        }
        migrate(&mut value, schema)?;
        serde_json::from_value(value).context("invalid manifest")
    }

    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("manifest always serializes");
        text.push('\n');
        text
    }
}

/// Upgrades a manifest from schema `from` to [`SCHEMA`], one version at a
/// time. Each future schema bump adds a step here, such as
/// `if from < 2 { migrate_1_to_2(value)?; }`, plus a test with a fixture of
/// the old format.
fn migrate(value: &mut serde_json::Value, from: u32) -> Result<()> {
    if from < 1 {
        anyhow::bail!("manifest schema {from} is not a valid version");
    }
    value["schema"] = SCHEMA.into();
    Ok(())
}

// ---------------------------------------------------------------------------
// Project folder
// ---------------------------------------------------------------------------

/// A project folder on disk.
#[derive(Debug, Clone)]
pub struct ProjectDir {
    pub root: PathBuf,
}

impl ProjectDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    pub fn proposed_clips_path(&self) -> PathBuf {
        self.root.join("candidates").join("clips.proposed.json")
    }

    pub fn is_project(path: &Path) -> bool {
        path.join(MANIFEST_FILE).is_file()
    }

    /// Creates the folder layout and writes the manifest. Fails if a
    /// project already exists there.
    pub fn create(root: impl Into<PathBuf>, manifest: &Manifest) -> Result<Self> {
        let dir = Self::new(root);
        if Self::is_project(&dir.root) {
            anyhow::bail!("a project already exists at {}", dir.root.display());
        }
        for sub in [
            "source",
            "analysis/frames",
            "candidates",
            "selections",
            "clips",
            "export",
        ] {
            let path = dir.root.join(sub);
            std::fs::create_dir_all(&path)
                .with_context(|| format!("creating {}", path.display()))?;
        }
        dir.save(manifest)?;
        Ok(dir)
    }

    pub fn load(&self) -> Result<Manifest> {
        let path = self.manifest_path();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Manifest::from_json(&text).with_context(|| format!("reading {}", path.display()))
    }

    pub fn save(&self, manifest: &Manifest) -> Result<()> {
        paths::write_atomic(&self.manifest_path(), manifest.to_json().as_bytes())
    }
}

/// Finds a project from a `--project` value: a path to a project folder, or
/// an id under `<workspace>/projects`. With no value, uses the current folder
/// if it is a project.
pub fn resolve(projects_dir: &Path, arg: Option<&str>) -> Result<Option<ProjectDir>> {
    let Some(arg) = arg else {
        let cwd = std::env::current_dir().context("reading the current folder")?;
        return Ok(ProjectDir::is_project(&cwd).then(|| ProjectDir::new(cwd)));
    };
    let as_path = PathBuf::from(arg);
    if ProjectDir::is_project(&as_path) {
        return Ok(Some(ProjectDir::new(as_path)));
    }
    let by_id = projects_dir.join(arg);
    if ProjectDir::is_project(&by_id) {
        return Ok(Some(ProjectDir::new(by_id)));
    }
    Err(
        JcError::new(ErrorKind::ProjectNotFound, format!("no project \"{arg}\""))
            .with_hint(format!(
                "pass a project folder or an id from {} (`jc status` lists them)",
                projects_dir.display()
            ))
            .into(),
    )
}

/// Like [`resolve`], but a project is required.
pub fn require(projects_dir: &Path, arg: Option<&str>) -> Result<ProjectDir> {
    resolve(projects_dir, arg)?.ok_or_else(|| {
        JcError::new(
            ErrorKind::ProjectNotFound,
            "no project given and the current folder is not a project",
        )
        .with_hint("pass --project <id|folder>")
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest::new(
            "2026-11-21_twitch_streamer_v123456",
            Source {
                kind: SourceKind::TwitchVod,
                url: Some("https://www.twitch.tv/videos/123456".into()),
                path: None,
                channel: Some("streamer".into()),
                title: Some("Long stream".into()),
                duration_s: Some(10834.0),
                rights: Rights {
                    clipping_for: Some("streamer".into()),
                    notes: String::new(),
                },
            },
        )
    }

    #[test]
    fn round_trips() {
        let mut m = sample();
        m.set_stage(Stage::Ingest, StageStatus::Done, None);
        let back = Manifest::from_json(&m.to_json()).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.stages.ingest.status, StageStatus::Done);
        assert_eq!(back.source.kind, SourceKind::TwitchVod);
    }

    #[test]
    fn stages_serialize_in_pipeline_order() {
        let json = sample().to_json();
        let positions: Vec<usize> = Stage::ALL
            .iter()
            .map(|s| json.find(&format!("\"{}\"", s.as_str())).unwrap())
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn rejects_newer_schema() {
        let mut v: serde_json::Value = serde_json::from_str(&sample().to_json()).unwrap();
        v["schema"] = (SCHEMA + 1).into();
        let err = Manifest::from_json(&v.to_string()).unwrap_err();
        assert!(err.to_string().contains("only knows up to"), "{err}");
    }

    #[test]
    fn rejects_missing_schema() {
        let mut v: serde_json::Value = serde_json::from_str(&sample().to_json()).unwrap();
        v.as_object_mut().unwrap().remove("schema");
        assert!(Manifest::from_json(&v.to_string()).is_err());
    }

    #[test]
    fn keeps_unknown_stage_fields() {
        let mut v: serde_json::Value = serde_json::from_str(&sample().to_json()).unwrap();
        v["stages"]["analyze"] = serde_json::json!({
            "status": "done",
            "signals": ["transcript", "chat"]
        });
        let back = Manifest::from_json(&v.to_string()).unwrap();
        assert_eq!(
            back.stages.analyze.extra["signals"],
            serde_json::json!(["transcript", "chat"])
        );
        assert!(back.to_json().contains("\"signals\""));
    }

    #[test]
    fn missing_stages_default_to_pending() {
        let mut v: serde_json::Value = serde_json::from_str(&sample().to_json()).unwrap();
        v["stages"] = serde_json::json!({ "ingest": { "status": "done" } });
        let back = Manifest::from_json(&v.to_string()).unwrap();
        assert_eq!(back.stages.first_unfinished(), Some(Stage::Analyze));
    }

    #[test]
    fn first_unfinished_skips_done_stages() {
        let mut m = sample();
        assert_eq!(m.stages.first_unfinished(), Some(Stage::Ingest));
        for s in Stage::ALL {
            m.set_stage(s, StageStatus::Done, None);
        }
        assert_eq!(m.stages.first_unfinished(), None);
    }

    #[test]
    fn create_load_and_refuse_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p1");
        let dir = ProjectDir::create(&root, &sample()).unwrap();
        assert!(root.join("analysis/frames").is_dir());
        assert_eq!(dir.load().unwrap().id, sample().id);
        assert!(!root.join("project.json.tmp").exists());
        assert!(ProjectDir::create(&root, &sample()).is_err());
    }

    #[test]
    fn resolve_by_id_and_path() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let root = projects.join("p1");
        ProjectDir::create(&root, &sample()).unwrap();

        let by_id = resolve(&projects, Some("p1")).unwrap().unwrap();
        assert_eq!(by_id.root, root);
        let by_path = resolve(&projects, Some(root.to_str().unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(by_path.root, root);

        let err = resolve(&projects, Some("nope")).unwrap_err();
        assert_eq!(crate::error::classify(&err).0, ErrorKind::ProjectNotFound);
    }
}
