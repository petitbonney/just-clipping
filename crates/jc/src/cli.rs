//! Command-line interface. Every command accepts `--json` for machine-readable
//! output on stdout (used by the `/clip` skill).

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "jc",
    version,
    about = "just-clipping: turn long videos and livestreams into short vertical clips",
    propagate_version = true
)]
pub struct Cli {
    /// Print the result as JSON on stdout
    #[arg(long, global = true)]
    pub json: bool,

    /// Workspace folder holding projects and config.toml [default: ~/JustClipping]
    #[arg(long, global = true, env = crate::paths::WORKSPACE_ENV, value_name = "DIR")]
    pub workspace: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check and install yt-dlp and ffmpeg, report the GPU backend
    Doctor(DoctorArgs),
    /// Show where a project is in the pipeline, or list all projects
    Status(StatusArgs),
    /// Create a project from a YouTube/Twitch URL or a local file and download it
    Ingest(IngestArgs),
    /// Transcribe and compute per-second signals (chat, audio, heatmap, scenes)
    Analyze(AnalyzeArgs),
    /// Find candidate moments in the analysis timeline
    Candidates(ProjectArgs),
    /// Open the local review page and wait for the user's picks
    Review(ReviewArgs),
    /// Cut 16:9 masters and render vertical clips with captions
    Render(ProjectArgs),
    /// Build cover candidates and render the chosen covers
    Package(ProjectArgs),
    /// Write the final folder for each clip
    Export(ProjectArgs),
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Print only problems
    #[arg(long, short)]
    pub quiet: bool,
    /// Report only; do not download or update anything
    #[arg(long)]
    pub check: bool,
    /// Check for a new yt-dlp release now, whatever its age
    #[arg(long, conflicts_with = "check")]
    pub update: bool,
}

#[derive(Debug, Args)]
pub struct ProjectArgs {
    /// Project id or folder [default: the current folder, if it is a project]
    #[arg(long, short)]
    pub project: Option<String>,
}

#[derive(Debug, Args)]
pub struct AnalyzeArgs {
    #[command(flatten)]
    pub project: ProjectArgs,
    /// Redo every step, even those already done
    #[arg(long)]
    pub force: bool,
    /// Spoken language code, such as en or fr [default: detected]
    #[arg(long)]
    pub language: Option<String>,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Project id or folder [default: the current folder, or list all projects]
    #[arg(long, short)]
    pub project: Option<String>,
}

#[derive(Debug, Args)]
pub struct IngestArgs {
    /// YouTube or Twitch URL, or a path to a local video file
    pub source: String,
    /// Import chat from a file: a TwitchDownloader JSON export, a yt-dlp
    /// YouTube .live_chat.json, or a chat.jsonl
    #[arg(long, value_name = "FILE", conflicts_with = "no_chat")]
    pub chat: Option<PathBuf>,
    /// Do not download chat
    #[arg(long)]
    pub no_chat: bool,
    /// Content profile (gaming, podcast, irl) [default: guessed from metadata]
    #[arg(long)]
    pub profile: Option<String>,
    /// Spoken language code, such as en or fr [default: detected during analyze]
    #[arg(long)]
    pub language: Option<String>,
    /// Who you are clipping for (recorded in the project)
    #[arg(long, value_name = "NAME")]
    pub clipping_for: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub project: ProjectArgs,
    /// Which picks to review
    #[arg(long, value_enum)]
    pub stage: ReviewStage,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReviewStage {
    Clips,
    Package,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
