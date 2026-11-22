//! `jc status`: where a project is in the pipeline and what to do next, or a
//! list of every project in the workspace.

use crate::output::{Report, json_of};
use crate::paths::Paths;
use crate::project::{self, Manifest, ProjectDir, Stage, StageStatus};
use anyhow::{Context, Result};
use serde::Serialize;
use std::fmt::Write as _;
use std::path::PathBuf;

/// Who acts next: jc runs a command, Claude makes a judgment call, or the
/// user picks on the review page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Jc,
    Claude,
    User,
}

#[derive(Debug, Clone, Serialize)]
pub struct NextStep {
    pub stage: Stage,
    pub actor: Actor,
    /// Set when the stage is `failed` or was left `running`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StageStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub description: String,
}

/// Works out the next step from the manifest, plus whether Claude has
/// already written `clips.proposed.json`.
pub fn next_step(manifest: &Manifest, has_proposed_clips: bool) -> Option<NextStep> {
    let stage = manifest.stages.first_unfinished()?;
    let record = manifest.stages.get(stage);
    let id = &manifest.id;
    let jc = |cmd: String, desc: &str| (Actor::Jc, Some(cmd), desc.to_string());
    let (actor, command, description) = match stage {
        Stage::Ingest => jc(
            format!("jc ingest {}", source_arg(manifest)),
            "download the source video, chat and metadata",
        ),
        Stage::Analyze => jc(
            format!("jc analyze -p {id}"),
            "transcribe and compute per-second signals",
        ),
        Stage::Candidates => jc(
            format!("jc candidates -p {id}"),
            "find candidate moments in the timeline",
        ),
        Stage::ReviewClips if !has_proposed_clips => (
            Actor::Claude,
            None,
            "rank and trim the candidates, then write candidates/clips.proposed.json".to_string(),
        ),
        Stage::ReviewClips => (
            Actor::User,
            Some(format!("jc review -p {id} --stage clips")),
            "pick clips on the review page".to_string(),
        ),
        Stage::Render => jc(format!("jc render -p {id}"), "render the selected clips"),
        Stage::Package => (
            Actor::Claude,
            Some(format!("jc package -p {id}")),
            "pick cover frames and write copy, then build the covers".to_string(),
        ),
        Stage::ReviewPackage => (
            Actor::User,
            Some(format!("jc review -p {id} --stage package")),
            "pick covers and copy on the review page".to_string(),
        ),
        Stage::Export => jc(format!("jc export -p {id}"), "write the export folders"),
    };
    let status = match record.status {
        StageStatus::Failed | StageStatus::Running => Some(record.status),
        _ => None,
    };
    Some(NextStep {
        stage,
        actor,
        status,
        command,
        description,
    })
}

fn source_arg(manifest: &Manifest) -> String {
    let source = &manifest.source;
    match (&source.url, &source.path) {
        (Some(url), _) => url.clone(),
        (None, Some(path)) => format!("\"{}\"", path.display()),
        (None, None) => "<source>".to_string(),
    }
}

// ---------------------------------------------------------------------------
// One project
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ProjectStatus {
    pub path: PathBuf,
    pub project: Manifest,
    pub next: Option<NextStep>,
}

impl Report for ProjectStatus {
    fn to_json(&self) -> serde_json::Value {
        json_of(self)
    }

    fn to_human(&self, _quiet: bool) -> String {
        let m = &self.project;
        let mut out = String::new();
        let _ = writeln!(out, "project   {}", m.id);
        let _ = writeln!(out, "folder    {}", self.path.display());
        let _ = writeln!(
            out,
            "source    {}  {}",
            m.source.kind.as_str(),
            source_arg(m)
        );
        if let Some(title) = &m.source.title {
            let _ = writeln!(out, "title     {title}");
        }
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
        let _ = writeln!(
            out,
            "language  {}   profile {}   style {}",
            opt(&m.language),
            opt(&m.profile),
            opt(&m.style)
        );
        let _ = writeln!(out, "clips     {}", m.clips.len());
        out.push('\n');
        for stage in Stage::ALL {
            let record = m.stages.get(stage);
            let at = record
                .at
                .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {:<8} {:<15} {}",
                record.status.as_str(),
                stage.as_str(),
                at
            );
            if let Some(err) = &record.error {
                let _ = writeln!(out, "           {err}");
            }
        }
        out.push('\n');
        match &self.next {
            None => out.push_str("next: nothing, this project is exported\n"),
            Some(next) => {
                let actor = match next.actor {
                    Actor::Jc => "jc",
                    Actor::Claude => "Claude",
                    Actor::User => "you",
                };
                let again = match next.status {
                    Some(StageStatus::Failed) => " (failed last time)",
                    Some(StageStatus::Running) => " (was interrupted)",
                    _ => "",
                };
                let _ = writeln!(out, "next: {actor}: {}{again}", next.description);
                if let Some(cmd) = &next.command {
                    let _ = writeln!(out, "      {cmd}");
                }
            }
        }
        out
    }
}

pub fn project_status(dir: &ProjectDir) -> Result<ProjectStatus> {
    let manifest = dir.load()?;
    let next = next_step(&manifest, dir.proposed_clips_path().is_file());
    Ok(ProjectStatus {
        path: dir.root.clone(),
        project: manifest,
        next,
    })
}

// ---------------------------------------------------------------------------
// All projects
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ProjectSummary {
    pub id: String,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `None` once exported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<NextStep>,
    /// Set when the manifest could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct WorkspaceStatus {
    pub workspace: PathBuf,
    pub projects: Vec<ProjectSummary>,
}

impl Report for WorkspaceStatus {
    fn to_json(&self) -> serde_json::Value {
        json_of(self)
    }

    fn to_human(&self, _quiet: bool) -> String {
        let mut out = String::new();
        if self.projects.is_empty() {
            let _ = writeln!(
                out,
                "No projects in {} yet. Start one with `jc ingest <url|file>`.",
                self.workspace.display()
            );
            return out;
        }
        let _ = writeln!(
            out,
            "{} projects in {}\n",
            self.projects.len(),
            self.workspace.display()
        );
        for p in &self.projects {
            let state = match (&p.error, &p.next) {
                (Some(err), _) => format!("unreadable: {err}"),
                (None, Some(next)) => format!("next: {}", next.stage.as_str()),
                (None, None) => "exported".to_string(),
            };
            let _ = writeln!(out, "  {:<44} {}", p.id, state);
            if let Some(title) = &p.title {
                let _ = writeln!(out, "    {title}");
            }
        }
        out
    }
}

pub fn workspace_status(paths: &Paths) -> Result<WorkspaceStatus> {
    let projects_dir = paths.projects_dir();
    let mut projects = Vec::new();
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(entries) => Some(entries),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", projects_dir.display())),
    };
    for entry in entries.into_iter().flatten() {
        let path = entry
            .with_context(|| format!("reading {}", projects_dir.display()))?
            .path();
        if !ProjectDir::is_project(&path) {
            continue;
        }
        let id = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = ProjectDir::new(&path);
        projects.push(match project_status(&dir) {
            Ok(s) => ProjectSummary {
                id: s.project.id,
                title: s.project.source.title,
                next: s.next,
                path,
                error: None,
            },
            Err(e) => ProjectSummary {
                id,
                path,
                title: None,
                next: None,
                error: Some(format!("{e:#}")),
            },
        });
    }
    // Ids start with the date, so newest first.
    projects.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(WorkspaceStatus {
        workspace: paths.workspace.clone(),
        projects,
    })
}

pub fn run(paths: &Paths, project_arg: Option<&str>) -> Result<Box<dyn Report>> {
    Ok(
        match project::resolve(&paths.projects_dir(), project_arg)? {
            Some(dir) => Box::new(project_status(&dir)?),
            None => Box::new(workspace_status(paths)?),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Rights, Source, SourceKind};

    fn manifest() -> Manifest {
        Manifest::new(
            "p1",
            Source {
                kind: SourceKind::Local,
                url: None,
                path: Some("/videos/a.mp4".into()),
                channel: None,
                title: None,
                duration_s: None,
                video_id: None,
                video: None,
                rights: Rights::default(),
            },
        )
    }

    fn done_until(m: &mut Manifest, stage: Stage) {
        for s in Stage::ALL.into_iter().take_while(|&s| s != stage) {
            m.set_stage(s, StageStatus::Done, None);
        }
    }

    #[test]
    fn new_project_starts_with_ingest() {
        let next = next_step(&manifest(), false).unwrap();
        assert_eq!(next.stage, Stage::Ingest);
        assert_eq!(next.actor, Actor::Jc);
        assert_eq!(next.command.unwrap(), "jc ingest \"/videos/a.mp4\"");
    }

    #[test]
    fn claude_ranks_before_clip_review() {
        let mut m = manifest();
        done_until(&mut m, Stage::ReviewClips);
        let next = next_step(&m, false).unwrap();
        assert_eq!(
            (next.stage, next.actor),
            (Stage::ReviewClips, Actor::Claude)
        );
        let next = next_step(&m, true).unwrap();
        assert_eq!((next.stage, next.actor), (Stage::ReviewClips, Actor::User));
        assert_eq!(next.command.unwrap(), "jc review -p p1 --stage clips");
    }

    #[test]
    fn failed_stage_is_retried() {
        let mut m = manifest();
        done_until(&mut m, Stage::Analyze);
        m.set_stage(Stage::Analyze, StageStatus::Failed, Some("boom".into()));
        let next = next_step(&m, false).unwrap();
        assert_eq!(next.stage, Stage::Analyze);
        assert_eq!(next.status, Some(StageStatus::Failed));
    }

    #[test]
    fn exported_project_has_no_next_step() {
        let mut m = manifest();
        for s in Stage::ALL {
            m.set_stage(s, StageStatus::Done, None);
        }
        assert!(next_step(&m, true).is_none());
    }
}
