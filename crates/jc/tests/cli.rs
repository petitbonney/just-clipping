//! End-to-end tests of the `jc` binary. No network: doctor runs with
//! `--check` against an empty cache.

use assert_cmd::Command;
use jc::project::{Manifest, ProjectDir, Rights, Source, SourceKind, Stage, StageStatus};
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

struct Env {
    workspace: TempDir,
    cache: TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            workspace: TempDir::new().unwrap(),
            cache: TempDir::new().unwrap(),
        }
    }

    fn jc(&self) -> Command {
        let mut cmd = Command::cargo_bin("jc").unwrap();
        cmd.env("JC_WORKSPACE", self.workspace.path())
            .env("JC_CACHE_DIR", self.cache.path())
            // Never pick up a project from the folder the tests run in.
            .current_dir(self.workspace.path());
        cmd
    }

    fn add_project(&self, id: &str) -> ProjectDir {
        let mut manifest = Manifest::new(
            id,
            Source {
                kind: SourceKind::YoutubeVod,
                url: Some("https://www.youtube.com/watch?v=abc".into()),
                path: None,
                channel: Some("chan".into()),
                title: Some("A long video".into()),
                duration_s: Some(3600.0),
                video_id: None,
                video: None,
                rights: Rights::default(),
            },
        );
        manifest.set_stage(Stage::Ingest, StageStatus::Done, None);
        ProjectDir::create(self.workspace.path().join("projects").join(id), &manifest).unwrap()
    }
}

fn json(output: &[u8]) -> serde_json::Value {
    serde_json::from_slice(output).expect("stdout is JSON")
}

#[test]
fn status_of_empty_workspace() {
    let env = Env::new();
    let out = env.jc().args(["status", "--json"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(json(&out.stdout)["projects"], serde_json::json!([]));

    env.jc()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("No projects"));
}

#[test]
fn status_lists_projects_newest_first() {
    let env = Env::new();
    env.add_project("2026-11-01_youtube_chan_a");
    env.add_project("2026-11-20_youtube_chan_b");
    let out = env.jc().args(["status", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v = json(&out.stdout);
    let ids: Vec<&str> = v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["2026-11-20_youtube_chan_b", "2026-11-01_youtube_chan_a"]
    );
    assert_eq!(v["projects"][0]["next"]["stage"], "analyze");
}

#[test]
fn status_of_one_project_by_id_and_from_its_folder() {
    let env = Env::new();
    let dir = env.add_project("p1");

    let out = env
        .jc()
        .args(["status", "-p", "p1", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v = json(&out.stdout);
    assert_eq!(v["project"]["id"], "p1");
    assert_eq!(v["project"]["stages"]["ingest"]["status"], "done");
    assert_eq!(v["next"]["actor"], "jc");
    assert_eq!(v["next"]["command"], "jc analyze -p p1");

    env.jc()
        .arg("status")
        .current_dir(&dir.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("next: jc: transcribe"));
}

#[test]
fn unknown_project_exits_5() {
    let env = Env::new();
    let out = env
        .jc()
        .args(["status", "-p", "nope", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(json(&out.stdout)["error"]["code"], "project_not_found");
}

#[test]
fn unreadable_manifest_is_listed_not_fatal() {
    let env = Env::new();
    env.add_project("good");
    let bad = env.workspace.path().join("projects/bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("project.json"), "{ not json").unwrap();

    let out = env.jc().args(["status", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v = json(&out.stdout);
    let bad = v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "bad")
        .unwrap();
    assert!(bad["error"].as_str().unwrap().contains("invalid JSON"));
}

#[test]
fn unimplemented_commands_exit_3() {
    let env = Env::new();
    for args in [
        vec!["candidates"],
        vec!["review", "--stage", "clips"],
        vec!["render"],
        vec!["package"],
        vec!["export"],
    ] {
        let out = env.jc().args(&args).arg("--json").output().unwrap();
        assert_eq!(out.status.code(), Some(3), "{args:?}");
        assert_eq!(json(&out.stdout)["error"]["code"], "not_implemented");
    }
}

#[test]
fn bad_arguments_exit_2() {
    let env = Env::new();
    env.jc()
        .args(["review", "--stage", "nope"])
        .assert()
        .code(2);
}

#[test]
fn doctor_check_reports_missing_tools_without_downloading() {
    let env = Env::new();
    let out = env
        .jc()
        .args(["doctor", "--check", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let v = json(&out.stdout);
    assert_eq!(v["ok"], false);
    let yt_dlp = tool(&v, "yt-dlp");
    assert_eq!(yt_dlp["name"], "yt-dlp");
    assert_eq!(yt_dlp["status"], "missing");
    assert!(
        is_empty(env.cache.path()),
        "--check must not write to the cache"
    );
}

#[test]
fn doctor_uses_config_paths_as_given() {
    let env = Env::new();
    std::fs::write(
        env.workspace.path().join("config.toml"),
        "[tools]\nffmpeg = \"/nonexistent/ffmpeg\"\n",
    )
    .unwrap();
    let out = env
        .jc()
        .args(["doctor", "--check", "--json"])
        .output()
        .unwrap();
    let v = json(&out.stdout);
    let ffmpeg = tool(&v, "ffmpeg");
    assert_eq!(ffmpeg["name"], "ffmpeg");
    assert_eq!(ffmpeg["status"], "missing");
    assert!(
        ffmpeg["message"]
            .as_str()
            .unwrap()
            .contains("/nonexistent/ffmpeg")
    );
}

fn tool<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in the doctor report"))
}

#[test]
fn doctor_check_lists_models() {
    let env = Env::new();
    let out = env
        .jc()
        .args(["doctor", "--check", "--json"])
        .output()
        .unwrap();
    let v = json(&out.stdout);
    for name in ["deno", "whisper", "vad", "yunet"] {
        assert_eq!(tool(&v, name)["status"], "missing", "{name}");
    }
    assert_eq!(tool(&v, "whisper")["version"], "large-v3-turbo-q5_0");
}

#[test]
fn unknown_whisper_model_is_an_error() {
    let env = Env::new();
    std::fs::write(
        env.workspace.path().join("config.toml"),
        "[models]\nwhisper = \"huge\"\n",
    )
    .unwrap();
    let out = env
        .jc()
        .args(["doctor", "--check", "--json"])
        .output()
        .unwrap();
    let whisper = tool(&json(&out.stdout), "whisper").clone();
    assert_eq!(whisper["status"], "error");
    assert!(whisper["message"].as_str().unwrap().contains("tiny"));
}

#[test]
fn ingest_rejects_unsupported_sources() {
    let env = Env::new();
    for (arg, needle) in [
        ("https://vimeo.com/123", "not a supported URL"),
        ("https://www.twitch.tv/somechannel", "channel"),
        (
            "no-such-file.mp4",
            "not a supported URL or an existing file",
        ),
    ] {
        let out = env.jc().args(["ingest", arg, "--json"]).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{arg}");
        let msg = json(&out.stdout)["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.contains(needle), "{arg}: {msg}");
    }
}

#[test]
fn ingest_without_tools_points_to_doctor() {
    let env = Env::new();
    std::fs::write(
        env.workspace.path().join("config.toml"),
        "[tools]\nprefer_system_ffmpeg = false\n",
    )
    .unwrap();
    let video = env.workspace.path().join("clip.mp4");
    std::fs::write(&video, b"not really a video").unwrap();
    let out = env
        .jc()
        .args(["ingest", video.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let v = json(&out.stdout);
    assert_eq!(v["error"]["code"], "setup_incomplete");
    assert!(v["error"]["hint"].as_str().unwrap().contains("jc doctor"));
}

#[test]
fn analyze_needs_an_ingested_project() {
    let env = Env::new();
    let out = env.jc().args(["analyze", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(5), "no project");

    let mut manifest = Manifest::new(
        "fresh",
        Source {
            kind: SourceKind::Local,
            url: None,
            path: Some("/x.mp4".into()),
            channel: None,
            title: None,
            duration_s: None,
            video_id: None,
            video: None,
            rights: Rights::default(),
        },
    );
    manifest.set_stage(Stage::Ingest, StageStatus::Failed, Some("boom".into()));
    ProjectDir::create(env.workspace.path().join("projects/fresh"), &manifest).unwrap();
    let out = env
        .jc()
        .args(["analyze", "-p", "fresh", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let msg = json(&out.stdout)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(msg.contains("not ingested"), "{msg}");
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}
