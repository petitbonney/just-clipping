//! Languages and content profiles, defined as TOML data.
//!
//! The files in the repo's `data/` folder are built into the binary. A file
//! with the same name in `<workspace>/data/lang/` or
//! `<workspace>/data/profiles/` replaces the built-in one, and a new name adds
//! a language or profile, without recompiling.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

macro_rules! builtin {
    ($($dir:literal / $name:literal),* $(,)?) => {
        &[$(($dir, $name, include_str!(concat!("../../../data/", $dir, "/", $name, ".toml")))),*]
    };
}

const BUILTIN: &[(&str, &str, &str)] = builtin![
    "lang" / "en",
    "lang" / "fr",
    "profiles" / "gaming",
    "profiles" / "podcast",
    "profiles" / "irl",
];

#[derive(Debug, Clone, Deserialize)]
pub struct Lang {
    pub code: String,
    pub name: String,
    pub whisper_code: String,
    #[serde(default = "default_segmentation")]
    pub segmentation: String,
    #[serde(default)]
    pub fillers: Vec<String>,
    #[serde(default)]
    pub chat_hype: Vec<String>,
    #[serde(default)]
    pub typography: Typography,
    #[serde(default)]
    pub caption: Caption,
}

fn default_segmentation() -> String {
    "whitespace".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Typography {
    #[serde(default)]
    pub nbsp_before: Vec<String>,
    #[serde(default)]
    pub nbsp_after: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Caption {
    pub max_chars_line: u32,
    pub font: String,
}

impl Default for Caption {
    fn default() -> Self {
        Self {
            max_chars_line: 18,
            font: "Montserrat-ExtraBold".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub name: String,
    pub layouts: Vec<String>,
    pub duration: Duration,
    pub weights: Weights,
    #[serde(default)]
    pub chat_emotes: Vec<String>,
    #[serde(default)]
    pub detect: Detect,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Duration {
    pub min_s: f64,
    pub max_s: f64,
    pub target_s: f64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Weights {
    #[serde(default)]
    pub chat_rate: f64,
    #[serde(default)]
    pub chat_hype: f64,
    #[serde(default)]
    pub heatmap: f64,
    #[serde(default)]
    pub audio: f64,
    #[serde(default)]
    pub scenes: f64,
}

/// How ingest guesses a profile from source metadata.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Detect {
    #[serde(default)]
    pub youtube_categories: Vec<String>,
    /// Twitch categories; `"*"` matches any category no profile names.
    #[serde(default)]
    pub twitch_games: Vec<String>,
    /// Words in the title that point to this profile.
    #[serde(default)]
    pub title_words: Vec<String>,
}

/// Every known language and profile.
#[derive(Debug, Clone)]
pub struct Data {
    pub langs: BTreeMap<String, Lang>,
    pub profiles: BTreeMap<String, Profile>,
}

impl Data {
    /// Built-in data only.
    pub fn builtin() -> Result<Self> {
        Self::load(None)
    }

    /// Built-in data, overridden by `<workspace>/data` when given.
    pub fn load(workspace_data: Option<&Path>) -> Result<Self> {
        let mut files: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
        for (dir, name, text) in BUILTIN {
            files.insert(
                (dir.to_string(), name.to_string()),
                (format!("built-in {dir}/{name}.toml"), text.to_string()),
            );
        }
        if let Some(root) = workspace_data {
            for dir in ["lang", "profiles"] {
                let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "toml")
                        && let Some(stem) = path.file_stem()
                    {
                        let text = std::fs::read_to_string(&path)
                            .with_context(|| format!("reading {}", path.display()))?;
                        files.insert(
                            (dir.to_string(), stem.to_string_lossy().into_owned()),
                            (path.display().to_string(), text),
                        );
                    }
                }
            }
        }

        let mut data = Data {
            langs: BTreeMap::new(),
            profiles: BTreeMap::new(),
        };
        for ((dir, name), (origin, text)) in files {
            if dir == "lang" {
                let lang: Lang =
                    toml::from_str(&text).with_context(|| format!("reading {origin}"))?;
                data.langs.insert(name, lang);
            } else {
                let profile: Profile =
                    toml::from_str(&text).with_context(|| format!("reading {origin}"))?;
                data.profiles.insert(name, profile);
            }
        }
        Ok(data)
    }

    pub fn lang(&self, code: &str) -> Option<&Lang> {
        self.langs.get(code)
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    /// Guesses a profile from source metadata. `None` when nothing points
    /// anywhere, so Claude or the user decides.
    pub fn detect_profile(&self, hints: &ProfileHints) -> Option<String> {
        let title = hints.title.to_lowercase();
        let title_words: Vec<&str> = title
            .split(|c: char| !c.is_alphanumeric() && c != '.')
            .filter(|w| !w.is_empty())
            .collect();
        for (name, p) in &self.profiles {
            if p.detect
                .title_words
                .iter()
                .any(|w| title_words.contains(&w.to_lowercase().as_str()))
            {
                return Some(name.clone());
            }
        }
        if let Some(game) = &hints.twitch_game {
            let exact = self.profiles.iter().find(|(_, p)| {
                p.detect
                    .twitch_games
                    .iter()
                    .any(|g| g.eq_ignore_ascii_case(game))
            });
            let wildcard = || {
                self.profiles
                    .iter()
                    .find(|(_, p)| p.detect.twitch_games.iter().any(|g| g == "*"))
            };
            if let Some((name, _)) = exact.or_else(wildcard) {
                return Some(name.clone());
            }
        }
        for category in &hints.youtube_categories {
            if let Some((name, _)) = self.profiles.iter().find(|(_, p)| {
                p.detect
                    .youtube_categories
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(category))
            }) {
                return Some(name.clone());
            }
        }
        None
    }
}

/// Source metadata used by [`Data::detect_profile`].
#[derive(Debug, Default)]
pub struct ProfileHints {
    pub title: String,
    pub youtube_categories: Vec<String>,
    pub twitch_game: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_data_parses() {
        let data = Data::builtin().unwrap();
        assert_eq!(data.lang("fr").unwrap().whisper_code, "fr");
        assert!(
            data.lang("en")
                .unwrap()
                .chat_hype
                .contains(&"lol".to_string())
        );
        assert_eq!(data.profile("gaming").unwrap().duration.target_s, 35.0);
        assert_eq!(data.profiles.len(), 3);
    }

    #[test]
    fn workspace_files_override_and_add() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("lang")).unwrap();
        std::fs::write(
            tmp.path().join("lang/es.toml"),
            "code = \"es\"\nname = \"Español\"\nwhisper_code = \"es\"\nchat_hype = [\"jaja\"]\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("lang/fr.toml"),
            "code = \"fr\"\nname = \"Français (perso)\"\nwhisper_code = \"fr\"\n",
        )
        .unwrap();
        let data = Data::load(Some(tmp.path())).unwrap();
        assert_eq!(data.lang("es").unwrap().chat_hype, ["jaja"]);
        assert_eq!(data.lang("fr").unwrap().name, "Français (perso)");
        assert!(data.lang("en").is_some());
    }

    #[test]
    fn detects_profiles() {
        let data = Data::builtin().unwrap();
        let detect = |title: &str, yt: &[&str], game: Option<&str>| {
            data.detect_profile(&ProfileHints {
                title: title.into(),
                youtube_categories: yt.iter().map(|s| s.to_string()).collect(),
                twitch_game: game.map(String::from),
            })
        };
        assert_eq!(
            detect("Elden Ring part 3", &[], Some("Elden Ring")).as_deref(),
            Some("gaming")
        );
        assert_eq!(
            detect("chill stream", &[], Some("Just Chatting")).as_deref(),
            Some("irl")
        );
        assert_eq!(detect("Run", &["Gaming"], None).as_deref(), Some("gaming"));
        assert_eq!(
            detect("Le Podcast #12", &["Entertainment"], None).as_deref(),
            Some("podcast")
        );
        assert_eq!(detect("Vlog", &["Entertainment"], None), None);
    }
}
