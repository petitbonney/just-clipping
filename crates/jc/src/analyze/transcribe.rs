//! Speech to text with word timestamps (whisper.cpp through whisper-rs).
//!
//! Audio is decoded by ffmpeg to 16 kHz mono and handled in 10-minute
//! chunks. Silero VAD finds the speech in each chunk; only the speech is sent
//! to whisper (faster, and whisper cannot invent text over silence or music),
//! and the times are mapped back to the video. Speech running into the end
//! of a chunk is carried into the next one, so chunks are always cut in
//! silence. Finished segments are appended to
//! `transcript.partial.jsonl`, so an interrupted run resumes where it
//! stopped instead of starting over.

use crate::output;
use crate::tools::Ffmpeg;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext,
    WhisperVadContextParams, WhisperVadParams,
};

const SAMPLE_RATE: usize = 16_000;
const CHUNK_S: usize = 600;
/// Segments more likely silence than speech are dropped (whisper invents
/// text over music and silence).
const MAX_NO_SPEECH: f32 = 0.6;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub t0: f64,
    pub t1: f64,
    pub text: String,
    /// Confidence, 0..1 (lowest token probability in the word).
    pub p: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub t0: f64,
    pub t1: f64,
    pub text: String,
    pub words: Vec<Word>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Transcript {
    pub language: String,
    pub model: String,
    pub duration_s: f64,
    pub segments: Vec<Segment>,
}

pub struct Options<'a> {
    pub model_path: &'a Path,
    /// Silero VAD model: only speech is transcribed.
    pub vad_model_path: &'a Path,
    pub model_id: &'a str,
    /// `None` detects the language on the first chunk.
    pub language: Option<String>,
    pub threads: u32,
    pub beam_size: u32,
}

// ---------------------------------------------------------------------------
// Words from tokens
// ---------------------------------------------------------------------------

/// A decoded token: text, start and end in seconds, probability.
#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub t0: f64,
    pub t1: f64,
    pub p: f64,
}

/// Joins sub-word tokens into words. A token starting with a space starts a
/// new word; anything else (word pieces, punctuation) extends the current
/// one. Times are clamped to the segment and kept in order.
pub fn words_from_tokens(tokens: &[Token], seg_t0: f64, seg_t1: f64) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    for tok in tokens {
        if tok.text.is_empty() {
            continue;
        }
        let starts_word = tok.text.starts_with(' ') || words.is_empty();
        let t0 = tok.t0.clamp(seg_t0, seg_t1);
        let t1 = tok.t1.clamp(t0, seg_t1);
        if starts_word && !tok.text.trim().is_empty() {
            words.push(Word {
                t0,
                t1,
                text: tok.text.trim().to_string(),
                p: tok.p,
            });
        } else if let Some(w) = words.last_mut() {
            w.text.push_str(tok.text.trim());
            w.t1 = w.t1.max(t1);
            w.p = w.p.min(tok.p);
        }
    }
    let mut prev_end = seg_t0;
    for w in &mut words {
        w.t0 = w.t0.max(prev_end);
        w.t1 = w.t1.max(w.t0);
        w.t0 = round2(w.t0);
        w.t1 = round2(w.t1);
        w.p = (w.p * 1000.0).round() / 1000.0;
        prev_end = w.t1;
    }
    words.retain(|w| !w.text.is_empty());
    words
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// Resumable partial output
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PartialHeader {
    model: String,
    language: String,
}

fn read_partial(path: &Path, model_id: &str) -> Option<(String, Vec<Segment>)> {
    let file = std::fs::File::open(path).ok()?;
    let mut lines = BufReader::new(file).lines();
    let header: PartialHeader = serde_json::from_str(&lines.next()?.ok()?).ok()?;
    if header.model != model_id {
        return None;
    }
    let mut segments = Vec::new();
    for line in lines.map_while(Result::ok) {
        // A torn last line from a crash is simply redone.
        match serde_json::from_str(&line) {
            Ok(s) => segments.push(s),
            Err(_) => break,
        }
    }
    Some((header.language, segments))
}

struct PartialWriter {
    file: std::fs::File,
}

impl PartialWriter {
    fn create(path: &Path, model: &str, language: &str, existing: &[Segment]) -> Result<Self> {
        let mut file =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let header = PartialHeader {
            model: model.into(),
            language: language.into(),
        };
        writeln!(file, "{}", serde_json::to_string(&header)?)?;
        let mut w = Self { file };
        w.append(existing)?;
        Ok(w)
    }

    fn append(&mut self, segments: &[Segment]) -> Result<()> {
        for s in segments {
            writeln!(self.file, "{}", serde_json::to_string(s)?)?;
        }
        self.file.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Transcription
// ---------------------------------------------------------------------------

/// Streams 16 kHz mono f32 samples from ffmpeg, starting at `start_s`.
struct Decoder {
    child: std::process::Child,
    stdout: std::process::ChildStdout,
    /// Bytes of a sample split across two reads.
    carry: Vec<u8>,
    eof: bool,
}

impl Decoder {
    fn start(ffmpeg: &Ffmpeg, video: &Path, start_s: f64) -> Result<Self> {
        let mut child = Command::new(&ffmpeg.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
            .args(["-ss", &format!("{start_s:.3}"), "-i"])
            .arg(video)
            .args(["-vn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("starting ffmpeg to decode audio")?;
        let stdout = child.stdout.take().expect("stdout is piped");
        Ok(Self {
            child,
            stdout,
            carry: Vec::new(),
            eof: false,
        })
    }

    /// Appends samples to `buf` until it holds `want` samples or the audio
    /// ends.
    fn fill(&mut self, buf: &mut Vec<f32>, want: usize) -> Result<()> {
        let mut bytes = [0u8; 64 * 1024];
        while buf.len() < want && !self.eof {
            let n = self
                .stdout
                .read(&mut bytes)
                .context("reading decoded audio")?;
            if n == 0 {
                self.eof = true;
                break;
            }
            self.carry.extend_from_slice(&bytes[..n]);
            let (samples, rest) = self.carry.as_chunks::<4>();
            buf.extend(samples.iter().map(|b| f32::from_le_bytes(*b)));
            let whole = self.carry.len() - rest.len();
            self.carry.drain(..whole);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        let status = self.child.wait()?;
        if !status.success() {
            bail!("ffmpeg failed while decoding audio ({status})");
        }
        Ok(())
    }
}

fn threads(configured: u32) -> i32 {
    if configured > 0 {
        return configured as i32;
    }
    std::thread::available_parallelism()
        .map(|n| n.get().min(8) as i32)
        .unwrap_or(4)
}

pub fn transcribe(
    ffmpeg: &Ffmpeg,
    video: &Path,
    duration_s: f64,
    opts: &Options,
    partial_path: &Path,
) -> Result<Transcript> {
    whisper_rs::install_logging_hooks();
    let ctx = WhisperContext::new_with_params(opts.model_path, WhisperContextParameters::default())
        .with_context(|| format!("loading {}", opts.model_path.display()))?;
    let mut state = ctx.create_state().context("creating whisper state")?;
    let eot = ctx.token_eot();
    let mut vad = {
        let mut p = WhisperVadContextParams::new();
        p.set_n_threads(threads(opts.threads).min(4));
        WhisperVadContext::new(
            opts.vad_model_path
                .to_str()
                .context("VAD model path is not UTF-8")?,
            p,
        )
        .context("loading the VAD model")?
    };

    let (mut language, mut segments) = match read_partial(partial_path, opts.model_id) {
        Some((lang, segs)) if opts.language.as_ref().is_none_or(|l| *l == lang) => {
            if let Some(last) = segs.last() {
                output::log(format!(
                    "Resuming the transcript at {}",
                    crate::chat::twitch::hms(last.t1)
                ));
            }
            (Some(lang), segs)
        }
        _ => (opts.language.clone(), Vec::new()),
    };
    let mut writer: Option<PartialWriter> = match &language {
        Some(lang) => Some(PartialWriter::create(
            partial_path,
            opts.model_id,
            lang,
            &segments,
        )?),
        None => None,
    };

    let start_s = segments.last().map_or(0.0, |s| s.t1);
    let mut decoder = Decoder::start(ffmpeg, video, start_s)?;
    let mut buf: Vec<f32> = Vec::new();
    let mut buf_start = start_s;
    let started = Instant::now();
    let chunk = CHUNK_S * SAMPLE_RATE;

    loop {
        decoder.fill(&mut buf, chunk)?;
        if buf.len() < SAMPLE_RATE / 10 {
            break;
        }
        let at_end = decoder.eof;
        let buf_s = buf.len() as f64 / SAMPLE_RATE as f64;

        // 1. Where is the speech in this chunk?
        let mut regions = speech_regions(&mut vad, &buf)?;

        // 2. Speech running into the end of the chunk is redone with the
        //    next one, so chunks are always cut in silence.
        let mut keep_from_s = buf_s;
        if !at_end
            && let Some(last) = regions.last()
            && last.1 >= buf_s - 0.5
            && last.0 >= 30.0
        {
            keep_from_s = last.0;
            regions.pop();
        }

        // 3. Transcribe only the speech, then map times back to the video.
        let (batch, pieces) = speech_batch(&buf, &regions);
        let mut found = Vec::new();
        if !batch.is_empty() {
            let mut params = FullParams::new(if opts.beam_size > 1 {
                SamplingStrategy::BeamSearch {
                    beam_size: opts.beam_size as i32,
                    patience: -1.0,
                }
            } else {
                SamplingStrategy::Greedy { best_of: 1 }
            });
            params.set_n_threads(threads(opts.threads));
            params.set_language(Some(language.as_deref().unwrap_or("auto")));
            params.set_token_timestamps(true);
            params.set_no_context(true);
            params.set_suppress_nst(true);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_special(false);
            params.set_print_timestamps(false);
            let (chunk_start, total) = (buf_start, duration_s);
            let mut last_shown = -1i32;
            let t_started = started;
            params.set_progress_callback_safe(move |pct: i32| {
                if pct / 10 != last_shown / 10 {
                    last_shown = pct;
                    let at = chunk_start + keep_from_s * pct as f64 / 100.0;
                    let speed = (at - start_s) / t_started.elapsed().as_secs_f64().max(1e-3);
                    output::log(format!(
                        "  transcribing {} / {} ({:.0}%), {speed:.1}x realtime",
                        crate::chat::twitch::hms(at),
                        crate::chat::twitch::hms(total),
                        100.0 * at / total.max(1.0),
                    ));
                }
            });
            state.full(params, &batch).context("whisper failed")?;

            if language.is_none() {
                let id = state.full_lang_id_from_state();
                let lang = whisper_rs::get_lang_str(id).unwrap_or("en").to_string();
                output::log(format!("  detected language: {lang}"));
                writer = Some(PartialWriter::create(
                    partial_path,
                    opts.model_id,
                    &lang,
                    &segments,
                )?);
                language = Some(lang);
            }

            let to_video = |cs: i64| -> f64 {
                (buf_start + map_time(&pieces, cs as f64 / 100.0)).min(duration_s)
            };
            for seg in state.as_iter() {
                if seg.no_speech_probability() > MAX_NO_SPEECH {
                    continue;
                }
                let t0 = to_video(seg.start_timestamp());
                let t1 = to_video(seg.end_timestamp()).max(t0);
                if t1 <= t0 {
                    continue;
                }
                let mut tokens = Vec::new();
                for j in 0..seg.n_tokens() {
                    let Some(tok) = seg.get_token(j) else {
                        continue;
                    };
                    if tok.token_id() >= eot {
                        continue;
                    }
                    let data = tok.token_data();
                    tokens.push(Token {
                        text: tok
                            .to_str_lossy()
                            .map(|c| c.into_owned())
                            .unwrap_or_default(),
                        t0: to_video(data.t0),
                        t1: to_video(data.t1),
                        p: data.p as f64,
                    });
                }
                let words = words_from_tokens(&tokens, t0, t1);
                let text = seg
                    .to_str_lossy()
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default();
                if words.is_empty() || text.is_empty() {
                    continue;
                }
                found.push(Segment {
                    t0: round2(t0),
                    t1: round2(t1),
                    text,
                    words,
                });
            }
        }

        let keep_from = ((keep_from_s * SAMPLE_RATE as f64) as usize).min(buf.len());
        if let Some(w) = writer.as_mut() {
            w.append(&found)?;
        }
        segments.extend(found);
        buf.drain(..keep_from);
        buf_start += keep_from as f64 / SAMPLE_RATE as f64;
        if at_end && buf.is_empty() {
            break;
        }
    }
    decoder.finish()?;

    Ok(Transcript {
        language: language.unwrap_or_else(|| "en".into()),
        model: opts.model_id.to_string(),
        duration_s,
        segments,
    })
}

// ---------------------------------------------------------------------------
// Speech regions
// ---------------------------------------------------------------------------

/// Silence between two speech regions when they are joined for whisper.
const GAP_S: f64 = 0.3;

/// Speech regions in `samples`, in seconds from its start.
fn speech_regions(vad: &mut WhisperVadContext, samples: &[f32]) -> Result<Vec<(f64, f64)>> {
    let mut params = WhisperVadParams::new();
    params.set_threshold(0.5);
    params.set_min_speech_duration(250);
    params.set_min_silence_duration(300);
    // Bounded regions keep the carry-over between chunks short.
    params.set_max_speech_duration(25.0);
    params.set_speech_pad(200);
    let segs = vad
        .segments_from_samples(params, samples)
        .context("voice activity detection failed")?;
    Ok(segs
        .map(|s| (s.start as f64 / 100.0, s.end as f64 / 100.0))
        .filter(|(a, b)| b > a)
        .collect())
}

/// Where a run of joined speech came from: `len` seconds starting at
/// `batch_t0` in the joined audio and at `orig_t0` in the chunk.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Piece {
    batch_t0: f64,
    orig_t0: f64,
    len: f64,
}

/// Joins the speech regions of `buf` with short silences in between.
fn speech_batch(buf: &[f32], regions: &[(f64, f64)]) -> (Vec<f32>, Vec<Piece>) {
    let mut batch = Vec::new();
    let mut pieces = Vec::new();
    let gap = vec![0.0f32; (GAP_S * SAMPLE_RATE as f64) as usize];
    for &(a, b) in regions {
        let lo = ((a * SAMPLE_RATE as f64) as usize).min(buf.len());
        let hi = ((b * SAMPLE_RATE as f64) as usize).min(buf.len());
        if hi <= lo {
            continue;
        }
        if !batch.is_empty() {
            batch.extend_from_slice(&gap);
        }
        pieces.push(Piece {
            batch_t0: batch.len() as f64 / SAMPLE_RATE as f64,
            orig_t0: lo as f64 / SAMPLE_RATE as f64,
            len: (hi - lo) as f64 / SAMPLE_RATE as f64,
        });
        batch.extend_from_slice(&buf[lo..hi]);
    }
    (batch, pieces)
}

/// Maps a time in the joined speech back to the chunk. Times in a gap snap
/// to the end of the piece before it.
fn map_time(pieces: &[Piece], t: f64) -> f64 {
    let Some(first) = pieces.first() else {
        return t;
    };
    let piece = pieces
        .iter()
        .rev()
        .find(|p| p.batch_t0 <= t)
        .unwrap_or(first);
    piece.orig_t0 + (t - piece.batch_t0).clamp(0.0, piece.len)
}

/// Path of the resumable partial transcript next to `transcript.json`.
pub fn partial_path(analysis_dir: &Path) -> PathBuf {
    analysis_dir.join("transcript.partial.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str, t0: f64, t1: f64) -> Token {
        Token {
            text: text.into(),
            t0,
            t1,
            p: 0.9,
        }
    }

    #[test]
    fn joins_pieces_and_punctuation() {
        let toks = [
            tok(" Bon", 1.0, 1.2),
            tok("jour", 1.2, 1.4),
            tok(" à", 1.5, 1.6),
            tok(" tous", 1.6, 1.9),
            tok(" !", 1.9, 2.0),
            tok(",", 2.0, 2.0),
        ];
        let words = words_from_tokens(&toks, 1.0, 2.0);
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Bonjour", "à", "tous", "!,"]);
        assert_eq!((words[0].t0, words[0].t1), (1.0, 1.4));
    }

    #[test]
    fn clamps_to_segment_and_keeps_order() {
        let toks = [
            tok(" a", 0.5, 3.0),
            tok(" b", 1.0, 1.5),
            tok(" c", 9.0, 9.5),
        ];
        let words = words_from_tokens(&toks, 1.0, 2.0);
        assert_eq!((words[0].t0, words[0].t1), (1.0, 2.0));
        assert!(words[1].t0 >= words[0].t1);
        assert_eq!((words[2].t0, words[2].t1), (2.0, 2.0));
    }

    #[test]
    fn joins_speech_and_maps_times_back() {
        let buf = vec![0.5f32; 10 * SAMPLE_RATE];
        let (batch, pieces) = speech_batch(&buf, &[(1.0, 2.0), (5.0, 6.5)]);
        assert_eq!(
            batch.len(),
            ((1.0 + GAP_S + 1.5) * SAMPLE_RATE as f64) as usize
        );
        assert_eq!(pieces[1].batch_t0, 1.0 + GAP_S);
        assert_eq!(map_time(&pieces, 0.5), 1.5);
        assert!((map_time(&pieces, 1.0 + GAP_S + 1.0) - 6.0).abs() < 1e-9);
        // In the gap: end of the first piece.
        assert_eq!(map_time(&pieces, 1.1), 2.0);
        assert_eq!(map_time(&[], 3.0), 3.0);
    }

    #[test]
    fn partial_round_trip_and_model_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jsonl");
        let seg = Segment {
            t0: 0.0,
            t1: 1.0,
            text: "salut".into(),
            words: vec![Word {
                t0: 0.0,
                t1: 1.0,
                text: "salut".into(),
                p: 0.9,
            }],
        };
        let mut w = PartialWriter::create(&path, "tiny", "fr", std::slice::from_ref(&seg)).unwrap();
        w.append(std::slice::from_ref(&seg)).unwrap();
        let (lang, segs) = read_partial(&path, "tiny").unwrap();
        assert_eq!((lang.as_str(), segs.len()), ("fr", 2));
        assert!(read_partial(&path, "base").is_none());
        // A torn last line is ignored.
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap() + "{\"t0\":").unwrap();
        assert_eq!(read_partial(&path, "tiny").unwrap().1.len(), 2);
    }
}
