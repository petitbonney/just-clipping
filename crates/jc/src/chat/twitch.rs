//! Twitch VOD chat and metadata through Twitch's internal GraphQL API.
//!
//! This API is unofficial: it is what twitch.tv itself uses, with the public
//! web client id, and Twitch can change it at any time. Every failure here is
//! reported as "no chat", never as a failed ingest, and `jc ingest --chat`
//! imports a TwitchDownloader export instead.

use super::ChatMessage;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const GQL_URL: &str = "https://gql.twitch.tv/gql";
/// The client id twitch.tv's own web player sends.
const WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const COMMENTS_QUERY_HASH: &str =
    "b70a3591ff0f4e0313d126c6a1502d79a1c02baebb288227c582044aa76adf6a";

pub struct TwitchGql {
    client: reqwest::blocking::Client,
}

#[derive(Debug, Default, Clone)]
pub struct VideoInfo {
    pub title: Option<String>,
    pub channel_login: Option<String>,
    pub game: Option<String>,
}

impl TwitchGql {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .user_agent(concat!("just-clipping/", env!("CARGO_PKG_VERSION")))
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    fn post(&self, body: &Value) -> Result<Value> {
        let mut last_err = None;
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(500 << attempt));
            }
            let resp = self
                .client
                .post(GQL_URL)
                .header("Client-ID", WEB_CLIENT_ID)
                .json(body)
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json::<Value>());
            match resp {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.expect("at least one attempt")).context("Twitch GraphQL request failed")
    }

    pub fn video_info(&self, video_id: &str) -> Result<VideoInfo> {
        let query =
            format!("query{{video(id:\"{video_id}\"){{title,owner{{login}},game{{name}}}}}}");
        let v = self.post(&json!({ "query": query }))?;
        let video = v
            .pointer("/data/video")
            .context("Twitch returned no video")?;
        if video.is_null() {
            bail!("Twitch video {video_id} not found (deleted, or subscriber-only)");
        }
        let s = |p: &str| video.pointer(p).and_then(Value::as_str).map(String::from);
        Ok(VideoInfo {
            title: s("/title"),
            channel_login: s("/owner/login"),
            game: s("/game/name"),
        })
    }

    fn comments_page(&self, video_id: &str, position: &Position) -> Result<Page> {
        let variables = match position {
            Position::Cursor(c) => json!({ "videoID": video_id, "cursor": c }),
            Position::Offset(o) => json!({ "videoID": video_id, "contentOffsetSeconds": o }),
        };
        let body = json!([{
            "operationName": "VideoCommentsByOffsetOrCursor",
            "variables": variables,
            "extensions": { "persistedQuery": { "version": 1, "sha256Hash": COMMENTS_QUERY_HASH } }
        }]);
        let v = self.post(&body)?;
        parse_comments_page(&v)
    }

    /// Downloads every chat message of a VOD. `duration_s` is only used for
    /// progress lines on stderr.
    pub fn comments(&self, video_id: &str, duration_s: Option<f64>) -> Result<Vec<ChatMessage>> {
        let mut out: Vec<ChatMessage> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut position = Position::Offset(0);
        let mut last_log = Instant::now();
        let mut use_cursor = true;
        let mut stuck_at: Option<u64> = None;

        loop {
            let page = match self.comments_page(video_id, &position) {
                Ok(p) => p,
                // Cursor paging has needed a client-integrity token at times;
                // offset paging is the fallback.
                Err(e) if matches!(position, Position::Cursor(_)) => {
                    crate::output::log(format!(
                        "  chat: cursor paging failed ({e:#}), switching to offset paging"
                    ));
                    use_cursor = false;
                    let last = out.last().map(|m| m.t as u64).unwrap_or(0);
                    position = Position::Offset(last);
                    continue;
                }
                Err(e) => return Err(e),
            };
            let before = out.len();
            for (id, msg) in page.messages {
                if seen.insert(id) {
                    out.push(msg);
                }
            }
            if last_log.elapsed() > Duration::from_secs(5) {
                let at = out.last().map(|m| m.t).unwrap_or(0.0);
                let total = duration_s
                    .map(|d| format!(" / {}", hms(d)))
                    .unwrap_or_default();
                crate::output::log(format!(
                    "  chat: {}{total}, {} messages",
                    hms(at),
                    out.len()
                ));
                last_log = Instant::now();
            }
            if !page.has_next {
                break;
            }
            position = match (&page.last_cursor, use_cursor) {
                (Some(c), true) => Position::Cursor(c.clone()),
                _ => {
                    // Offset paging: continue from the last message's second,
                    // skipping ahead when a page brought nothing new.
                    let last = out.last().map(|m| m.t as u64).unwrap_or(0);
                    let next = if out.len() == before {
                        let s = stuck_at.map_or(last, |s| s.max(last)) + 1;
                        stuck_at = Some(s);
                        s
                    } else {
                        stuck_at = None;
                        last
                    };
                    if duration_s.is_some_and(|d| next as f64 > d + 60.0) {
                        break;
                    }
                    Position::Offset(next)
                }
            };
            std::thread::sleep(Duration::from_millis(30));
        }
        Ok(out)
    }
}

enum Position {
    Cursor(String),
    Offset(u64),
}

struct Page {
    messages: Vec<(String, ChatMessage)>,
    has_next: bool,
    last_cursor: Option<String>,
}

fn parse_comments_page(v: &Value) -> Result<Page> {
    let first = v.get(0).unwrap_or(v);
    if let Some(errors) = first.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        let msgs: Vec<&str> = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(Value::as_str))
            .collect();
        bail!("Twitch GraphQL error: {}", msgs.join("; "));
    }
    let comments = first
        .pointer("/data/video/comments")
        .context("Twitch returned no comments (the VOD may be deleted or subscriber-only)")?;
    if comments.is_null() {
        bail!("Twitch returned no comments (the VOD may be deleted or subscriber-only)");
    }
    let edges = comments
        .get("edges")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut messages = Vec::with_capacity(edges.len());
    let mut last_cursor = None;
    for edge in &edges {
        if let Some(c) = edge.get("cursor").and_then(Value::as_str) {
            last_cursor = Some(c.to_string());
        }
        let Some(node) = edge.get("node") else {
            continue;
        };
        let Some(t) = node.get("contentOffsetSeconds").and_then(Value::as_f64) else {
            continue;
        };
        let id = node
            .get("id")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("{t}-{}", messages.len()));
        let user = node
            .pointer("/commenter/displayName")
            .or_else(|| node.pointer("/commenter/login"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut text = String::new();
        let mut emotes = Vec::new();
        for f in node
            .pointer("/message/fragments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let frag = f.get("text").and_then(Value::as_str).unwrap_or("");
            text.push_str(frag);
            if f.get("emote").is_some_and(|e| !e.is_null()) {
                emotes.push(frag.trim().to_string());
            }
        }
        messages.push((
            id,
            ChatMessage {
                t,
                user,
                text: text.trim().to_string(),
                emotes,
            },
        ));
    }
    let has_next = comments
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Page {
        messages,
        has_next,
        last_cursor,
    })
}

pub fn hms(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_comments_page() {
        let v: Value = serde_json::from_str(
            r#"[{"data":{"video":{"id":"1","comments":{"edges":[
              {"cursor":"c1","node":{"id":"m1","contentOffsetSeconds":10,"commenter":{"login":"a","displayName":"A"},
                "message":{"fragments":[{"text":"KEKW","emote":{"emoteID":"9"}},{"text":" lol","emote":null}]}}},
              {"cursor":"c2","node":{"id":"m2","contentOffsetSeconds":11,"commenter":null,
                "message":{"fragments":[{"text":"hi","emote":null}]}}}
            ],"pageInfo":{"hasNextPage":true}}}}}]"#,
        )
        .unwrap();
        let page = parse_comments_page(&v).unwrap();
        assert!(page.has_next);
        assert_eq!(page.last_cursor.as_deref(), Some("c2"));
        let (id, m) = &page.messages[0];
        assert_eq!(id, "m1");
        assert_eq!(
            (m.t, m.user.as_str(), m.text.as_str()),
            (10.0, "A", "KEKW lol")
        );
        assert_eq!(m.emotes, ["KEKW"]);
        assert_eq!(page.messages[1].1.user, "");
    }

    #[test]
    fn graphql_errors_are_errors() {
        let v: Value = serde_json::from_str(
            r#"[{"errors":[{"message":"failed integrity check"}],"data":null}]"#,
        )
        .unwrap();
        let err = parse_comments_page(&v).err().unwrap();
        assert!(err.to_string().contains("integrity"));
    }

    #[test]
    fn deleted_video_is_an_error() {
        let v: Value = serde_json::from_str(r#"[{"data":{"video":null}}]"#).unwrap();
        assert!(parse_comments_page(&v).is_err());
    }

    #[test]
    fn formats_hms() {
        assert_eq!(hms(3725.9), "1:02:05");
    }
}
