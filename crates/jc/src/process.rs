//! Running outside tools (yt-dlp, ffmpeg, ffprobe).
//!
//! A tool's own output never reaches jc's stdout, which carries only jc's
//! result. When a tool fails, the error includes the end of what it printed.

use anyhow::{Context, Result, bail};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

const TAIL_LINES: usize = 15;

fn describe(cmd: &Command) -> String {
    std::path::Path::new(cmd.get_program())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "command".into())
}

/// Runs `cmd`, showing its output on stderr as it goes.
pub fn run_logged(cmd: &mut Command) -> Result<()> {
    let name = describe(cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {name}"))?;
    let stderr = child.stderr.take().expect("stderr is piped");
    let tail = std::thread::spawn(move || {
        let mut tail = VecDeque::with_capacity(TAIL_LINES);
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("{line}");
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        tail
    });
    let status = child
        .wait()
        .with_context(|| format!("waiting for {name}"))?;
    let tail = tail.join().unwrap_or_default();
    if !status.success() {
        bail!("{name} failed ({status}):\n{}", Vec::from(tail).join("\n"));
    }
    Ok(())
}

/// Runs `cmd` and returns its stdout. stderr is kept for the error message.
pub fn output(cmd: &mut Command) -> Result<Vec<u8>> {
    let name = describe(cmd);
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {name}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(TAIL_LINES).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("{name} failed ({}):\n{}", out.status, tail.join("\n"));
    }
    Ok(out.stdout)
}

/// Runs `cmd`, calling `on_line` for each stderr line (ffmpeg writes its
/// filter logs there). stdout is discarded.
pub fn for_each_stderr_line(cmd: &mut Command, mut on_line: impl FnMut(&str)) -> Result<()> {
    let name = describe(cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {name}"))?;
    let stderr = child.stderr.take().expect("stderr is piped");
    let mut tail = VecDeque::with_capacity(TAIL_LINES);
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        on_line(&line);
        if tail.len() == TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    let status = child
        .wait()
        .with_context(|| format!("waiting for {name}"))?;
    if !status.success() {
        bail!("{name} failed ({status}):\n{}", Vec::from(tail).join("\n"));
    }
    Ok(())
}
