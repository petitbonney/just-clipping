//! Running tools to read their version and check their features.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// ffmpeg features the pipeline relies on, with what provides each.
pub const REQUIRED_FILTERS: [(&str, &str); 3] = [
    ("ass", "libass, for burning in captions"),
    ("ebur128", "loudness measurement"),
    ("loudnorm", "loudness normalization"),
];
pub const REQUIRED_ENCODERS: [(&str, &str); 2] = [
    ("libx264", "libx264, for H.264 video"),
    ("aac", "the native AAC audio encoder"),
];

fn stdout_of(bin: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("running {}", bin.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "{} {} exited with {}",
            bin.display(),
            args.join(" "),
            out.status
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The second column of `ffmpeg -filters` / `ffmpeg -encoders` lines.
fn listed_names(listing: &str) -> HashSet<&str> {
    listing
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect()
}

/// Features from [`REQUIRED_FILTERS`] and [`REQUIRED_ENCODERS`] that this
/// ffmpeg lacks, described for the user. Empty means it is usable.
pub fn ffmpeg_missing_features(ffmpeg: &Path) -> Result<Vec<String>> {
    let filters = stdout_of(ffmpeg, &["-hide_banner", "-filters"])?;
    let encoders = stdout_of(ffmpeg, &["-hide_banner", "-encoders"])?;
    Ok(missing_from(&filters, &encoders))
}

fn missing_from(filters: &str, encoders: &str) -> Vec<String> {
    let filters = listed_names(filters);
    let encoders = listed_names(encoders);
    REQUIRED_FILTERS
        .iter()
        .filter(|(name, _)| !filters.contains(name))
        .chain(
            REQUIRED_ENCODERS
                .iter()
                .filter(|(name, _)| !encoders.contains(name)),
        )
        .map(|(name, what)| format!("{name} ({what})"))
        .collect()
}

/// `ffmpeg version 9.1-https://… Copyright …` → `9.1-https://…`
pub fn ffmpeg_version(bin: &Path) -> Option<String> {
    let out = stdout_of(bin, &["-version"]).ok()?;
    let first = out.lines().next()?;
    first.split_whitespace().nth(2).map(str::to_string)
}

/// `yt-dlp --version` → `2026.10.26`
pub fn yt_dlp_version(bin: &Path) -> Option<String> {
    let out = stdout_of(bin, &["--version"]).ok()?;
    out.lines().next().map(|l| l.trim().to_string())
}

/// Days since a yt-dlp version date (`2026.10.26`, or `2026.10.26.1` for a
/// same-day patch release), or `None` if it is not a date.
pub fn yt_dlp_version_age_days(version: &str, today: jiff::civil::Date) -> Option<i64> {
    let mut parts = version.split('.');
    let year: i16 = parts.next()?.parse().ok()?;
    let month: i8 = parts.next()?.parse().ok()?;
    let day: i8 = parts.next()?.parse().ok()?;
    let date = jiff::civil::Date::new(year, month, day).ok()?;
    Some(today.duration_since(date).as_hours() / 24)
}

/// Looks `name` up on PATH, adding `.exe` on Windows.
pub fn which(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// True if `bin` runs and exits successfully with `args`.
pub fn runs(bin: &str, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILTERS: &str = "\
Filters:
  T.. = Timeline support
  ... = Slice threading
 ... abench            A->A       Benchmark part of a filtergraph.
 ... ebur128           A->N       EBU R128 scanner.
 ... loudnorm          A->A       EBU R128 loudness normalization
";
    const ENCODERS: &str = "\
Encoders:
 V..... = Video
 ------
 V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC
 A....D aac                  AAC (Advanced Audio Coding)
 A..... aac_at               aac (AudioToolbox) (codec aac)
";

    #[test]
    fn reports_missing_libass() {
        let missing = missing_from(FILTERS, ENCODERS);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].starts_with("ass "), "{missing:?}");
    }

    #[test]
    fn complete_build_has_nothing_missing() {
        let filters = format!("{FILTERS} T.C ass               V->V       Render ASS subtitles\n");
        assert!(missing_from(&filters, ENCODERS).is_empty());
    }

    #[test]
    fn aac_at_does_not_count_as_aac() {
        let encoders = ENCODERS.replace(
            " A....D aac                  AAC (Advanced Audio Coding)\n",
            "",
        );
        let missing = missing_from(FILTERS, &encoders);
        assert!(missing.iter().any(|m| m.starts_with("aac ")), "{missing:?}");
    }

    #[test]
    fn yt_dlp_age() {
        let today = jiff::civil::date(2026, 11, 22);
        assert_eq!(yt_dlp_version_age_days("2026.10.26", today), Some(27));
        assert_eq!(yt_dlp_version_age_days("2026.11.22.1", today), Some(0));
        assert_eq!(yt_dlp_version_age_days("nightly", today), None);
    }
}
