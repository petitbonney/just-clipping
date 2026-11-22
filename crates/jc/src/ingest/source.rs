//! Working out what a `jc ingest` argument points to, and naming projects.

use crate::error::{ErrorKind, JcError};
use anyhow::Result;
use reqwest::Url;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRef {
    Youtube { id: String },
    Twitch { video_id: String },
    Local { path: PathBuf },
}

impl SourceRef {
    /// The URL jc stores and passes to yt-dlp.
    pub fn canonical_url(&self) -> Option<String> {
        match self {
            SourceRef::Youtube { id } => Some(format!("https://www.youtube.com/watch?v={id}")),
            SourceRef::Twitch { video_id } => {
                Some(format!("https://www.twitch.tv/videos/{video_id}"))
            }
            SourceRef::Local { .. } => None,
        }
    }

    /// Platform video id, used to find an existing project for the same
    /// source (`v123456` for Twitch, as in Twitch's own URLs).
    pub fn video_id(&self) -> Option<String> {
        match self {
            SourceRef::Youtube { id } => Some(id.clone()),
            SourceRef::Twitch { video_id } => Some(format!("v{video_id}")),
            SourceRef::Local { .. } => None,
        }
    }

    pub fn platform(&self) -> &'static str {
        match self {
            SourceRef::Youtube { .. } => "youtube",
            SourceRef::Twitch { .. } => "twitch",
            SourceRef::Local { .. } => "local",
        }
    }
}

fn unsupported(msg: impl Into<String>, hint: &str) -> anyhow::Error {
    JcError::new(ErrorKind::Other, msg).with_hint(hint).into()
}

fn is_youtube_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Classifies a URL or file path.
pub fn classify(arg: &str) -> Result<SourceRef> {
    let path = Path::new(arg);
    if path.is_file() {
        let path = std::path::absolute(path)?;
        return Ok(SourceRef::Local { path });
    }

    let with_scheme = if arg.contains("://") {
        arg.to_string()
    } else {
        format!("https://{arg}")
    };
    let Ok(url) = Url::parse(&with_scheme) else {
        return Err(unsupported(
            format!("\"{arg}\" is not a file or a URL"),
            "pass a YouTube or Twitch video URL, or a path to a video file",
        ));
    };
    let host = url.host_str().unwrap_or("").trim_start_matches("www.");
    let host = host.trim_start_matches("m.");
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();

    match host {
        "youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {
            let id = match segments.as_slice() {
                ["watch"] => url
                    .query_pairs()
                    .find(|(k, _)| k == "v")
                    .map(|(_, v)| v.into_owned()),
                ["live" | "shorts" | "embed" | "v", id, ..] => Some(id.to_string()),
                _ => None,
            };
            match id {
                Some(id) if is_youtube_id(&id) => Ok(SourceRef::Youtube { id }),
                _ => Err(unsupported(
                    format!("{arg} is not a YouTube video URL"),
                    "use a video link such as https://www.youtube.com/watch?v=…",
                )),
            }
        }
        "youtu.be" => match segments.first() {
            Some(id) if is_youtube_id(id) => Ok(SourceRef::Youtube { id: id.to_string() }),
            _ => Err(unsupported(
                format!("{arg} is not a YouTube video URL"),
                "use a video link such as https://youtu.be/…",
            )),
        },
        "twitch.tv" => {
            let id = match segments.as_slice() {
                ["videos", id] => Some(*id),
                [_, "video" | "v", id] => Some(*id),
                _ => None,
            };
            match id {
                Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                    Ok(SourceRef::Twitch {
                        video_id: id.to_string(),
                    })
                }
                _ if segments.len() == 1 => Err(unsupported(
                    format!("{arg} is a Twitch channel, not a past broadcast"),
                    "live streams come with live mode (phase 2); for now pass a VOD link such as https://www.twitch.tv/videos/123456",
                )),
                _ => Err(unsupported(
                    format!("{arg} is not a Twitch VOD URL"),
                    "use a past broadcast link such as https://www.twitch.tv/videos/123456",
                )),
            }
        }
        _ => {
            if arg.contains("://") || !path.exists() {
                Err(unsupported(
                    format!("\"{arg}\" is not a supported URL or an existing file"),
                    "jc supports YouTube and Twitch video URLs and local video files",
                ))
            } else {
                Err(unsupported(
                    format!("{arg} is not a file"),
                    "pass a video file, not a folder",
                ))
            }
        }
    }
}

/// Lowercase ASCII letters, digits and single dashes, at most `max` chars.
pub fn slug(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let c = fold_accent(c);
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= max {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "untitled".into()
    } else {
        out
    }
}

fn fold_accent(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => 'a',
        'À' | 'Â' | 'Ä' | 'Á' | 'Ã' | 'Å' => 'A',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'î' | 'ï' | 'í' | 'ì' => 'i',
        'Î' | 'Ï' | 'Í' | 'Ì' => 'I',
        'ô' | 'ö' | 'ó' | 'ò' | 'õ' => 'o',
        'Ô' | 'Ö' | 'Ó' | 'Ò' | 'Õ' => 'O',
        'ù' | 'û' | 'ü' | 'ú' => 'u',
        'Ù' | 'Û' | 'Ü' | 'Ú' => 'U',
        'ç' => 'c',
        'Ç' => 'C',
        'ñ' => 'n',
        'Ñ' => 'N',
        other => other,
    }
}

/// `2026-11-21_twitch_streamer_v123456`, or `2026-11-21_local_my-video`.
pub fn project_id(date: jiff::civil::Date, source: &SourceRef, channel: Option<&str>) -> String {
    let date = date.strftime("%Y-%m-%d");
    match source {
        SourceRef::Local { path } => {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{date}_local_{}", slug(&stem, 40))
        }
        _ => format!(
            "{date}_{}_{}_{}",
            source.platform(),
            slug(channel.unwrap_or("unknown"), 30),
            source.video_id().unwrap_or_default()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt(id: &str) -> SourceRef {
        SourceRef::Youtube { id: id.into() }
    }

    #[test]
    fn youtube_urls() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
            "youtube.com/watch?v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "https://www.youtube.com/live/dQw4w9WgXcQ?feature=share",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
        ] {
            assert_eq!(classify(url).unwrap(), yt("dQw4w9WgXcQ"), "{url}");
        }
        assert!(classify("https://www.youtube.com/@somechannel").is_err());
        assert!(classify("https://www.youtube.com/watch?v=short").is_err());
    }

    #[test]
    fn twitch_urls() {
        let v = SourceRef::Twitch {
            video_id: "123456".into(),
        };
        assert_eq!(classify("https://www.twitch.tv/videos/123456").unwrap(), v);
        assert_eq!(classify("twitch.tv/videos/123456?t=1h2m").unwrap(), v);
        assert_eq!(classify("https://m.twitch.tv/videos/123456").unwrap(), v);
        assert_eq!(
            classify("https://www.twitch.tv/streamer/v/123456").unwrap(),
            v
        );
        let err = classify("https://www.twitch.tv/streamer").unwrap_err();
        assert!(err.to_string().contains("channel"), "{err}");
    }

    #[test]
    fn local_files_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("My Video.mkv");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            classify(file.to_str().unwrap()).unwrap(),
            SourceRef::Local { .. }
        ));
        assert!(classify(dir.path().to_str().unwrap()).is_err());
        assert!(classify("https://vimeo.com/123").is_err());
        assert!(classify("not a thing").is_err());
    }

    #[test]
    fn ids_and_slugs() {
        let date = jiff::civil::date(2026, 11, 21);
        let twitch = SourceRef::Twitch {
            video_id: "123456".into(),
        };
        assert_eq!(
            project_id(date, &twitch, Some("Streamer_FR")),
            "2026-11-21_twitch_streamer-fr_v123456"
        );
        let local = SourceRef::Local {
            path: "/x/Été à Paris (final).mp4".into(),
        };
        assert_eq!(
            project_id(date, &local, None),
            "2026-11-21_local_ete-a-paris-final"
        );
        assert_eq!(slug("!!!", 10), "untitled");
        assert_eq!(slug("abcdefghijk", 5), "abcde");
    }
}
