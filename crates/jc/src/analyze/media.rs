//! Signals read with ffmpeg: audio loudness (EBU R128) and scene changes,
//! plus YouTube's "most replayed" heatmap from info.json.

use super::signal;
use crate::process;
use crate::tools::Ffmpeg;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Quietest loudness kept, in LUFS. Silence reads as -120.
const FLOOR_LUFS: f64 = -70.0;
/// Rolling window for the normal loudness level.
const AUDIO_BASELINE_S: usize = 61;
/// A rise this many LU above normal scores 1.
const FULL_RISE_LU: f64 = 12.0;
/// scdet score (0-100) from which a frame counts as a cut.
const CUT_SCORE: f64 = 10.0;

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct AudioSignal {
    /// Loudest momentary loudness (400 ms window) in each second, LUFS.
    pub loudness: Vec<f64>,
    /// Rolling median of `loudness`: the normal level.
    pub baseline: Vec<f64>,
    /// 0..1: how far above normal (12 LU above = 1).
    pub score: Vec<f64>,
}

/// `[Parsed_ebur128_0 @ 0x…] t: 0.699955   TARGET:-23 LUFS    M: -16.0 S:…`
pub fn parse_ebur128_line(line: &str) -> Option<(f64, f64)> {
    let rest = &line[line.find("] t:")? + 4..];
    let t: f64 = rest.split_whitespace().next()?.parse().ok()?;
    let m_at = rest.find(" M:")? + 3;
    let m: f64 = rest[m_at..].split_whitespace().next()?.parse().ok()?;
    Some((t, m))
}

pub fn audio(ffmpeg: &Ffmpeg, video: &Path, duration_s: f64) -> Result<AudioSignal> {
    let n = signal::bins(duration_s);
    let mut loudness = vec![FLOOR_LUFS; n];
    process::for_each_stderr_line(
        Command::new(&ffmpeg.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-nostats", "-i"])
            .arg(video)
            .args(["-vn", "-af", "ebur128=peak=none", "-f", "null", "-"]),
        |line| {
            if let Some((t, m)) = parse_ebur128_line(line) {
                // M covers the 400 ms before t.
                let i = (t - 0.2).max(0.0) as usize;
                if i < n {
                    loudness[i] = loudness[i].max(m.max(FLOOR_LUFS));
                }
            }
        },
    )?;
    Ok(audio_from_loudness(loudness))
}

pub fn audio_from_loudness(loudness: Vec<f64>) -> AudioSignal {
    let baseline = signal::rolling_median(&loudness, AUDIO_BASELINE_S);
    let score: Vec<f64> = loudness
        .iter()
        .zip(&baseline)
        .map(|(l, b)| signal::clamp01((l - b) / FULL_RISE_LU))
        .collect();
    AudioSignal {
        loudness: signal::round(&loudness, 1),
        baseline: signal::round(&baseline, 1),
        score: signal::round(&score, 3),
    }
}

// ---------------------------------------------------------------------------
// Scenes
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct ScenesSignal {
    /// Times of hard cuts, in seconds.
    pub cuts: Vec<f64>,
    /// 0..1: biggest picture change in each second.
    pub score: Vec<f64>,
}

/// `[Parsed_scdet_2 @ 0x…] lavfi.scd.score: 23.4, lavfi.scd.time: 12.75`
pub fn parse_scdet_line(line: &str) -> Option<(f64, f64)> {
    let s_at = line.find("lavfi.scd.score:")? + "lavfi.scd.score:".len();
    let score: f64 = line[s_at..].split(',').next()?.trim().parse().ok()?;
    let t_at = line.find("lavfi.scd.time:")? + "lavfi.scd.time:".len();
    let t: f64 = line[t_at..].split_whitespace().next()?.parse().ok()?;
    Some((t, score))
}

pub fn scenes(ffmpeg: &Ffmpeg, video: &Path, duration_s: f64) -> Result<ScenesSignal> {
    let n = signal::bins(duration_s);
    let mut score = vec![0.0f64; n];
    let mut cuts = Vec::new();
    process::for_each_stderr_line(
        Command::new(&ffmpeg.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-nostats", "-i"])
            .arg(video)
            // Four small frames a second are plenty to see cuts, and much
            // cheaper to compare than full frames.
            .args([
                "-an",
                "-vf",
                "fps=4,scale=320:-2,scdet=threshold=1",
                "-f",
                "null",
                "-",
            ]),
        |line| {
            if let Some((t, s)) = parse_scdet_line(line) {
                let i = t as usize;
                if i < n {
                    score[i] = score[i].max(s / 100.0);
                }
                if s >= CUT_SCORE {
                    cuts.push((t * 100.0).round() / 100.0);
                }
            }
        },
    )?;
    Ok(ScenesSignal {
        cuts,
        score: signal::round(
            &score
                .iter()
                .map(|s| signal::clamp01(s * 4.0))
                .collect::<Vec<_>>(),
            3,
        ),
    })
}

// ---------------------------------------------------------------------------
// Heatmap
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct Heatmap {
    /// 0..1 per second, 1 = the most replayed part of the video.
    pub values: Vec<f64>,
}

/// Spreads yt-dlp's `heatmap` ranges (`start_time`, `end_time`, `value`)
/// over seconds. `None` when the video has no heatmap.
pub fn heatmap(info: &Value, duration_s: f64) -> Option<Heatmap> {
    let points = info.get("heatmap")?.as_array()?;
    if points.is_empty() {
        return None;
    }
    let n = signal::bins(duration_s);
    let mut values = vec![0.0; n];
    for p in points {
        let (Some(a), Some(b), Some(v)) = (
            p.get("start_time").and_then(Value::as_f64),
            p.get("end_time").and_then(Value::as_f64),
            p.get("value").and_then(Value::as_f64),
        ) else {
            continue;
        };
        let lo = a.max(0.0) as usize;
        let hi = (b.ceil() as usize).min(n);
        for slot in values.iter_mut().take(hi).skip(lo) {
            *slot = f64::max(*slot, v);
        }
    }
    let max = values.iter().cloned().fold(0.0, f64::max);
    if max <= 0.0 {
        return None;
    }
    Some(Heatmap {
        values: signal::round(&values.iter().map(|v| v / max).collect::<Vec<_>>(), 3),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ebur128() {
        let line = "[Parsed_ebur128_0 @ 0x7fb211308f80] t: 0.699955   TARGET:-23 LUFS    M: -16.0 S:-120.7     I: -15.5 LUFS       LRA:   0.0 LU";
        assert_eq!(parse_ebur128_line(line), Some((0.699955, -16.0)));
        assert_eq!(
            parse_ebur128_line("[Parsed_ebur128_0 @ 0x1] Summary:"),
            None
        );
        assert_eq!(parse_ebur128_line("Input #0, mov"), None);
    }

    #[test]
    fn parses_scdet() {
        let line =
            "[Parsed_scdet_2 @ 0x7fe56a906200] lavfi.scd.score: 23.063, lavfi.scd.time: 12.75";
        assert_eq!(parse_scdet_line(line), Some((12.75, 23.063)));
        assert_eq!(parse_scdet_line("frame=  100"), None);
    }

    #[test]
    fn loud_moment_scores() {
        let mut l = vec![-30.0; 120];
        l[60] = -18.0;
        let a = audio_from_loudness(l);
        assert_eq!(a.score[60], 1.0);
        assert_eq!(a.score[10], 0.0);
    }

    #[test]
    fn heatmap_spreads_and_normalizes() {
        let info: Value = serde_json::from_str(
            r#"{"heatmap":[{"start_time":0,"end_time":5,"value":0.2},{"start_time":5,"end_time":10,"value":0.8}]}"#,
        )
        .unwrap();
        let h = heatmap(&info, 10.0).unwrap();
        assert_eq!(h.values.len(), 10);
        assert_eq!(h.values[0], 0.25);
        assert_eq!(h.values[9], 1.0);
        assert!(heatmap(&serde_json::json!({}), 10.0).is_none());
        assert!(heatmap(&serde_json::json!({"heatmap": []}), 10.0).is_none());
    }
}
