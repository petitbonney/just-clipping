//! Chat replay, normalized to `source/chat.jsonl`: one message per line,
//! `{"t": seconds into the video, "user", "text", "emotes": [names]}`.

pub mod twitch;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Seconds from the start of the video.
    pub t: f64,
    pub user: String,
    pub text: String,
    /// Platform emotes in the message (Twitch emote names, YouTube custom
    /// emoji shortcuts and standard emoji).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emotes: Vec<String>,
}

/// Where a project's chat came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatOrigin {
    YoutubeReplay,
    TwitchGql,
    TwitchDownloader,
    Jsonl,
}

/// Sorts by time and writes `chat.jsonl` atomically.
pub fn write_jsonl(path: &Path, messages: &mut [ChatMessage]) -> Result<()> {
    messages.sort_by(|a, b| a.t.total_cmp(&b.t));
    let tmp = crate::doctor::download::with_suffix(path, ".tmp");
    {
        let file =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let mut w = BufWriter::new(file);
        for m in messages.iter() {
            serde_json::to_writer(&mut w, m)?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

pub fn read_jsonl(path: &Path) -> Result<Vec<ChatMessage>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(&line)
                .with_context(|| format!("{} line {}", path.display(), i + 1))?,
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// YouTube live chat replay (yt-dlp `--sub-langs live_chat`)
// ---------------------------------------------------------------------------

/// Parses one line of a yt-dlp `.live_chat.json` file. Returns the text
/// messages in it (super chats included, membership and system items not).
pub fn parse_youtube_line(line: &str) -> Result<Vec<ChatMessage>> {
    let v: Value = serde_json::from_str(line).context("invalid live chat JSON")?;
    let Some(replay) = v.get("replayChatItemAction") else {
        return Ok(Vec::new());
    };
    let offset_ms: f64 = replay
        .get("videoOffsetTimeMsec")
        .and_then(|o| {
            o.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| o.as_f64())
        })
        .unwrap_or(f64::NAN);
    let mut out = Vec::new();
    for action in replay
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(item) = action.pointer("/addChatItemAction/item") else {
            continue;
        };
        let renderer = item
            .get("liveChatTextMessageRenderer")
            .or_else(|| item.get("liveChatPaidMessageRenderer"));
        let Some(r) = renderer else { continue };
        if !offset_ms.is_finite() || offset_ms < 0.0 {
            continue;
        }
        let user = r
            .pointer("/authorName/simpleText")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let (text, emotes) = youtube_runs(r.pointer("/message/runs"));
        if text.trim().is_empty() && emotes.is_empty() {
            continue;
        }
        out.push(ChatMessage {
            t: offset_ms / 1000.0,
            user,
            text,
            emotes,
        });
    }
    Ok(out)
}

fn youtube_runs(runs: Option<&Value>) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut emotes = Vec::new();
    for run in runs.and_then(Value::as_array).into_iter().flatten() {
        if let Some(t) = run.get("text").and_then(Value::as_str) {
            text.push_str(t);
        } else if let Some(emoji) = run.get("emoji") {
            let custom = emoji
                .get("isCustomEmoji")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let shortcut = emoji
                .pointer("/shortcuts/0")
                .and_then(Value::as_str)
                .map(|s| s.trim_matches(':').to_string());
            let id = emoji
                .get("emojiId")
                .and_then(Value::as_str)
                .map(String::from);
            // Standard emoji: the id is the emoji itself. Custom ones have an
            // opaque id, so use the shortcut name.
            let name = if custom {
                shortcut.or(id)
            } else {
                id.or(shortcut)
            };
            if let Some(name) = name {
                text.push_str(&name);
                emotes.push(name);
            }
        }
    }
    (text.trim().to_string(), emotes)
}

pub fn parse_youtube_file(path: &Path) -> Result<Vec<ChatMessage>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.extend(
            parse_youtube_line(&line)
                .with_context(|| format!("{} line {}", path.display(), i + 1))?,
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// TwitchDownloader JSON export
// ---------------------------------------------------------------------------

pub fn parse_twitch_downloader(v: &Value) -> Result<Vec<ChatMessage>> {
    let comments = v
        .get("comments")
        .and_then(Value::as_array)
        .context("no \"comments\" array: not a TwitchDownloader chat file")?;
    let mut out = Vec::with_capacity(comments.len());
    for c in comments {
        let Some(t) = c.get("content_offset_seconds").and_then(Value::as_f64) else {
            continue;
        };
        let user = c
            .pointer("/commenter/display_name")
            .or_else(|| c.pointer("/commenter/name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let message = c.get("message");
        let text = message
            .and_then(|m| m.get("body"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let emotes = message
            .and_then(|m| m.get("fragments"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|f| f.get("emoticon").is_some_and(|e| !e.is_null()))
            .filter_map(|f| f.get("text").and_then(Value::as_str))
            .map(|s| s.trim().to_string())
            .collect();
        out.push(ChatMessage {
            t,
            user,
            text,
            emotes,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Import (`jc ingest --chat <file>`)
// ---------------------------------------------------------------------------

/// Reads a chat file in any supported format: TwitchDownloader JSON, a
/// yt-dlp YouTube `.live_chat.json`, or jc's own `chat.jsonl`.
pub fn import(path: &Path) -> Result<(Vec<ChatMessage>, ChatOrigin)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if first_line.contains("\"replayChatItemAction\"") {
        return Ok((parse_youtube_file(path)?, ChatOrigin::YoutubeReplay));
    }
    if let Ok(v) = serde_json::from_str::<Value>(&text)
        && v.get("comments").is_some()
    {
        let msgs =
            parse_twitch_downloader(&v).with_context(|| format!("reading {}", path.display()))?;
        return Ok((msgs, ChatOrigin::TwitchDownloader));
    }
    if serde_json::from_str::<ChatMessage>(first_line).is_ok() {
        return Ok((read_jsonl(path)?, ChatOrigin::Jsonl));
    }
    bail!(
        "{} is not a chat file jc knows (TwitchDownloader JSON, YouTube .live_chat.json or chat.jsonl)",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const YT_LINE: &str = r#"{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatTextMessageRenderer":{"message":{"runs":[{"text":"no way "},{"emoji":{"emojiId":"😂","shortcuts":[":face_with_tears_of_joy:"],"isCustomEmoji":false}},{"text":" "},{"emoji":{"emojiId":"UCkszU2WH9gy1mb0dV-11UJg/abc","shortcuts":[":yt:"],"isCustomEmoji":true}}]},"authorName":{"simpleText":"Alice"},"timestampUsec":"1700000000000000"}}}}],"videoOffsetTimeMsec":"83500"}}"#;

    #[test]
    fn youtube_text_and_emoji() {
        let msgs = parse_youtube_line(YT_LINE).unwrap();
        assert_eq!(msgs.len(), 1);
        let m = &msgs[0];
        assert_eq!(m.t, 83.5);
        assert_eq!(m.user, "Alice");
        assert_eq!(m.text, "no way 😂 yt");
        assert_eq!(m.emotes, ["😂", "yt"]);
    }

    #[test]
    fn youtube_skips_non_messages_and_pre_stream_chat() {
        let membership = r#"{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatMembershipItemRenderer":{}}}}],"videoOffsetTimeMsec":"1000"}}"#;
        assert!(parse_youtube_line(membership).unwrap().is_empty());
        let early = YT_LINE.replace("83500", "-5000");
        assert!(parse_youtube_line(&early).unwrap().is_empty());
        assert!(parse_youtube_line(r#"{"other":1}"#).unwrap().is_empty());
    }

    #[test]
    fn twitch_downloader_export() {
        let v: Value = serde_json::from_str(
            r#"{"video":{},"comments":[
              {"content_offset_seconds":12.0,"commenter":{"display_name":"Bob","name":"bob"},
               "message":{"body":"KEKW that was close","fragments":[
                 {"text":"KEKW","emoticon":{"emoticon_id":"1"}},{"text":" that was close","emoticon":null}]}},
              {"content_offset_seconds":13.5,"commenter":{"name":"carl"},"message":{"body":"gg"}}
            ]}"#,
        )
        .unwrap();
        let msgs = parse_twitch_downloader(&v).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].emotes, ["KEKW"]);
        assert_eq!(msgs[1].user, "carl");
    }

    #[test]
    fn jsonl_round_trip_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.jsonl");
        let mut msgs = vec![
            ChatMessage {
                t: 5.0,
                user: "b".into(),
                text: "two".into(),
                emotes: vec![],
            },
            ChatMessage {
                t: 1.0,
                user: "a".into(),
                text: "one".into(),
                emotes: vec!["Pog".into()],
            },
        ];
        write_jsonl(&path, &mut msgs).unwrap();
        let back = read_jsonl(&path).unwrap();
        assert_eq!(back[0].text, "one");
        assert_eq!(back.len(), 2);
        let (imported, origin) = import(&path).unwrap();
        assert_eq!(origin, ChatOrigin::Jsonl);
        assert_eq!(imported, back);
    }

    #[test]
    fn import_detects_formats() {
        let dir = tempfile::tempdir().unwrap();
        let yt = dir.path().join("x.live_chat.json");
        std::fs::write(&yt, format!("{YT_LINE}\n{YT_LINE}\n")).unwrap();
        let (msgs, origin) = import(&yt).unwrap();
        assert_eq!((msgs.len(), origin), (2, ChatOrigin::YoutubeReplay));

        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{\"hello\": 1}").unwrap();
        assert!(import(&bad).is_err());
    }
}
