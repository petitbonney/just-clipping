//! Chat activity per second: how busy chat is compared with its normal
//! level, and how many messages are hype (laughter, shock, emotes).

use super::signal;
use crate::chat::ChatMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Rolling window for chat's normal level.
const BASELINE_WINDOW_S: usize = 301;
/// Reactions spread over a few seconds.
const SMOOTH_S: usize = 3;

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatSignal {
    /// Seconds chat was shifted back to line reactions up with their cause.
    pub delay_s: f64,
    pub messages: usize,
    /// Messages per second.
    pub rate: Vec<f64>,
    /// Rolling median of `rate`: chat's normal level.
    pub baseline: Vec<f64>,
    /// Hype messages per second.
    pub hype: Vec<f64>,
    /// 0..1: busy compared with normal (8x normal = 1).
    pub rate_score: Vec<f64>,
    /// 0..1: hype messages compared with normal chat volume.
    pub hype_score: Vec<f64>,
}

/// Matches hype words and emotes in chat messages.
pub struct HypeMatcher {
    words: HashSet<String>,
    phrases: Vec<String>,
    symbols: Vec<String>,
    emotes: HashSet<String>,
}

/// Lowercase, with any letter repeated 3+ times kept once ("mdrrrr" → "mdr").
fn squeeze(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let chars: Vec<char> = word.chars().flat_map(char::to_lowercase).collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let mut j = i;
        while j < chars.len() && chars[j] == c {
            j += 1;
        }
        let run = j - i;
        let keep = if run >= 3 { 1 } else { run };
        out.extend(std::iter::repeat_n(c, keep));
        i = j;
    }
    out
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(squeeze)
        .collect()
}

impl HypeMatcher {
    /// `lexicon`: words, phrases and emoji from the language files.
    /// `emotes`: exact emote names from the profile files.
    pub fn new<'a>(
        lexicon: impl IntoIterator<Item = &'a String>,
        emotes: impl IntoIterator<Item = &'a String>,
    ) -> Self {
        let mut words = HashSet::new();
        let mut phrases = Vec::new();
        let mut symbols = Vec::new();
        for entry in lexicon {
            let has_alnum = entry.chars().any(char::is_alphanumeric);
            if !has_alnum {
                symbols.push(entry.clone());
                continue;
            }
            let t = tokens(entry);
            match t.len() {
                0 => {}
                1 => {
                    words.insert(t[0].clone());
                }
                _ => phrases.push(format!(" {} ", t.join(" "))),
            }
        }
        Self {
            words,
            phrases,
            symbols,
            emotes: emotes.into_iter().cloned().collect(),
        }
    }

    pub fn is_hype(&self, m: &ChatMessage) -> bool {
        if m.emotes.iter().any(|e| self.emotes.contains(e)) {
            return true;
        }
        // Third-party emotes (7TV, BTTV) arrive as plain words.
        if m.text.split_whitespace().any(|w| self.emotes.contains(w)) {
            return true;
        }
        if self.symbols.iter().any(|s| m.text.contains(s.as_str())) {
            return true;
        }
        let toks = tokens(&m.text);
        if toks.iter().any(|t| self.words.contains(t)) {
            return true;
        }
        if !self.phrases.is_empty() {
            let joined = format!(" {} ", toks.join(" "));
            return self.phrases.iter().any(|p| joined.contains(p.as_str()));
        }
        false
    }
}

pub fn compute(
    messages: &[ChatMessage],
    duration_s: f64,
    delay_s: f64,
    matcher: &HypeMatcher,
) -> ChatSignal {
    let n = signal::bins(duration_s);
    let mut rate = vec![0.0; n];
    let mut hype = vec![0.0; n];
    for m in messages {
        let t = (m.t - delay_s).max(0.0);
        let i = t as usize;
        if i >= n {
            continue;
        }
        rate[i] += 1.0;
        if matcher.is_hype(m) {
            hype[i] += 1.0;
        }
    }
    let rate_s = signal::smooth(&rate, SMOOTH_S);
    let hype_s = signal::smooth(&hype, SMOOTH_S);
    // A floor keeps a near-silent chat from turning one message into a spike.
    let baseline: Vec<f64> = signal::rolling_median(&rate_s, BASELINE_WINDOW_S)
        .into_iter()
        .map(|b| b.max(0.2))
        .collect();
    let rate_score: Vec<f64> = rate_s
        .iter()
        .zip(&baseline)
        .map(|(r, b)| signal::ratio_score(r / b, 8.0))
        .collect();
    let hype_score: Vec<f64> = hype_s
        .iter()
        .zip(&baseline)
        .map(|(h, b)| signal::ratio_score(1.0 + h / b, 5.0))
        .collect();
    ChatSignal {
        delay_s,
        messages: messages.len(),
        rate: signal::round(&rate, 2),
        baseline: signal::round(&baseline, 2),
        hype: signal::round(&hype, 2),
        rate_score: signal::round(&rate_score, 3),
        hype_score: signal::round(&hype_score, 3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(t: f64, text: &str) -> ChatMessage {
        ChatMessage {
            t,
            user: "u".into(),
            text: text.into(),
            emotes: vec![],
        }
    }

    fn matcher() -> HypeMatcher {
        let lex: Vec<String> = ["mdr", "oh non", "lol", "💀"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let emotes: Vec<String> = ["KEKW".to_string()].into();
        HypeMatcher::new(&lex, &emotes)
    }

    #[test]
    fn matches_words_phrases_symbols_and_emotes() {
        let m = matcher();
        assert!(m.is_hype(&msg(0.0, "MDRRRR")));
        assert!(m.is_hype(&msg(0.0, "ptdr oh non pas ça")));
        assert!(m.is_hype(&msg(0.0, "💀💀")));
        assert!(m.is_hype(&msg(0.0, "KEKW")));
        assert!(!m.is_hype(&msg(0.0, "kekw")), "emotes are case-sensitive");
        assert!(!m.is_hype(&msg(0.0, "lolita")), "whole words only");
        assert!(!m.is_hype(&msg(0.0, "non oh")));
        let mut e = msg(0.0, "nice");
        e.emotes = vec!["KEKW".into()];
        assert!(m.is_hype(&e));
    }

    #[test]
    fn burst_scores_high_and_delay_shifts_it() {
        let mut msgs = Vec::new();
        for t in 0..600 {
            msgs.push(msg(t as f64 + 0.5, "salut"));
        }
        for i in 0..40 {
            msgs.push(msg(305.0 + (i % 4) as f64 * 0.25, "mdr"));
        }
        let s = compute(&msgs, 600.0, 5.0, &matcher());
        assert_eq!(s.rate.len(), 600);
        let max = s.rate_score.iter().cloned().fold(0.0, f64::max);
        assert_eq!(s.rate_score[300], max, "burst at 305s minus 5s delay");
        assert!(s.rate_score[310] < max);
        assert!(s.rate_score[300] > 0.8);
        assert!(s.hype_score[300] > 0.8);
        assert_eq!(s.rate_score[100], 0.0);
    }

    #[test]
    fn empty_chat_is_all_zero() {
        let s = compute(&[], 10.0, 5.0, &matcher());
        assert!(s.rate_score.iter().all(|v| *v == 0.0));
    }
}
