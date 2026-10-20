//! K-pop titles learned from the user's own sorting.
//!
//! When a frame is put in K-pop BY HAND, its title is something the built-in rules (`kpop.rs`)
//! missed — so the title is written here, and every other frame carrying it follows into K-pop at
//! the next save. The list is a file beside the sidecar, shown and editable on the Analysis panel:
//! a title learned by mistake is removed there, and is then remembered as DISMISSED so the same
//! hand-sorted frame does not teach it again at the next scan.
//!
//! Like the rest of the K-pop category this is a rule re-derived on every save, never a stored
//! decision: removing a title releases the frames it had taken.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::kpop::contains_bounded;

pub const LEARNED_FILE_NAME: &str = ".image-categorizer-kpop-learned.json";

/// Shorter than this and a title says too little to stand for a video ("mv", "official").
const MIN_KEY_CHARS: usize = 6;
const MIN_KEY_TOKENS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LearnKind {
    /// Learned from a frame: matches frames whose title is the same title, as normalized.
    Title,
    /// Typed on the panel: matches any title CONTAINING it — a name, like the terms file.
    Term,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnedTitle {
    /// The normalized form every comparison uses (see [`normalize`]).
    pub key: String,
    /// The title as it was read, for display.
    pub title: String,
    pub kind: LearnKind,
    /// Which reading it came from: "ocr" (the title band), "description" (the vision model's
    /// quote of it), or "typed".
    pub source: String,
    pub added_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnedFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub titles: Vec<LearnedTitle>,
    /// Keys removed on the panel. Never learned again from a hand-sorted frame; typing the title
    /// back in un-dismisses it.
    #[serde(default)]
    pub dismissed: Vec<String>,
}

pub fn learned_path(root: &Path) -> PathBuf {
    root.join(LEARNED_FILE_NAME)
}

pub fn load(root: &Path) -> LearnedFile {
    fs::read_to_string(learned_path(root))
        .ok()
        .and_then(|data| serde_json::from_str::<LearnedFile>(&data).ok())
        .unwrap_or_default()
}

pub fn save(root: &Path, file: &LearnedFile) -> Result<(), String> {
    let mut file = file.clone();
    file.version = 1;
    let data = serde_json::to_string_pretty(&file)
        .map_err(|error| format!("Failed to serialize learned K-pop titles: {error}"))?;
    fs::write(learned_path(root), data).map_err(|error| format!("Failed to save learned K-pop titles: {error}"))
}

/// The form two readings of one title share. Lowercased; the site suffix (" - YouTube") cut off;
/// everything that is not a letter or digit (Hangul included) turned to a space; and the leading
/// notification counts the band picks up off the tab ("9 (2) IVE …" → "ive …") dropped — they
/// change from one screenshot to the next while the title does not.
pub fn normalize(title: &str) -> String {
    let lower = title.to_lowercase();
    let cut = ["- youtube", "— youtube", "– youtube"]
        .iter()
        .filter_map(|suffix| lower.find(suffix))
        .min()
        .map_or(lower.as_str(), |index| &lower[..index]);
    let spaced: String = cut.chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    let mut tokens: Vec<&str> = spaced.split_whitespace().collect();
    while tokens.first().is_some_and(|token| token.len() <= 3 && token.chars().all(|c| c.is_ascii_digit())) {
        tokens.remove(0);
    }
    tokens.join(" ")
}

fn token_count(key: &str) -> usize {
    key.split(' ').filter(|token| !token.is_empty()).count()
}

/// Whether `key` is worth learning — long enough to stand for one video.
pub fn learnable(key: &str) -> bool {
    key.chars().count() >= MIN_KEY_CHARS && token_count(key) >= MIN_KEY_TOKENS
}

/// Matches a frame's normalized readings against the learned list.
pub struct LearnedMatcher<'a> {
    titles: &'a [LearnedTitle],
}

impl<'a> LearnedMatcher<'a> {
    pub fn new(titles: &'a [LearnedTitle]) -> Self {
        Self { titles }
    }

    pub fn is_empty(&self) -> bool {
        self.titles.is_empty()
    }

    /// The index of the first learned entry `frame_key` matches.
    ///
    /// A learned TITLE matches the same title, or one that contains it whole (the band read past
    /// the end: "… 'off the record' mv premium"), or — when the frame's reading is long enough to
    /// identify a video by itself — one it is a cut-off part of (the band lost the artist name:
    /// "moon dream dress special visual clip" inside "triples e21 moon dream dress …"). A typed
    /// TERM matches wherever it occurs as whole words.
    pub fn find(&self, frame_key: &str) -> Option<usize> {
        if frame_key.is_empty() {
            return None;
        }
        self.titles.iter().position(|entry| match entry.kind {
            LearnKind::Term => contains_bounded(frame_key, &entry.key),
            LearnKind::Title => {
                frame_key == entry.key
                    || (entry.key.chars().count() >= 12
                        && token_count(&entry.key) >= 3
                        && contains_words(frame_key, &entry.key))
                    || (frame_key.chars().count() >= 20
                        && token_count(frame_key) >= 4
                        && contains_words(&entry.key, frame_key))
            }
        })
    }
}

/// `needle` occurs in `haystack` as whole space-separated tokens.
fn contains_words(haystack: &str, needle: &str) -> bool {
    format!(" {haystack} ").contains(&format!(" {needle} "))
}

/// One hand-sorted K-pop frame's readings, offered to [`learn`].
pub struct HandSorted<'a> {
    pub hash: &'a str,
    pub path: &'a str,
    /// The OCR readings of the title band (English, Korean), each possibly empty.
    pub ocr: [&'a str; 2],
    /// The vision model's quote of the title, when it quoted ONE title (not a tab strip).
    pub described: &'a str,
}

/// Adds the titles of hand-sorted K-pop frames that are not on the list yet, skipping readings the
/// built-in rules already accept (`already_kpop`) — learning those would teach nothing and bury
/// the entries that matter. Returns what was added.
pub fn learn<'a>(
    file: &mut LearnedFile,
    frames: impl IntoIterator<Item = HandSorted<'a>>,
    already_kpop: impl Fn(&str) -> bool,
    now: &str,
) -> Vec<LearnedTitle> {
    let mut added = Vec::new();
    for frame in frames {
        let readings = [(frame.ocr[0], "ocr"), (frame.ocr[1], "ocr"), (frame.described, "description")];
        for (reading, source) in readings {
            let reading = reading.trim();
            if reading.is_empty() || already_kpop(reading) {
                continue;
            }
            let key = normalize(reading);
            if !learnable(&key)
                || file.dismissed.iter().any(|dismissed| dismissed == &key)
                || LearnedMatcher::new(&file.titles).find(&key).is_some()
            {
                continue;
            }
            let entry = LearnedTitle {
                key,
                title: reading.to_string(),
                kind: LearnKind::Title,
                source: source.to_string(),
                added_at: now.to_string(),
                from_hash: Some(frame.hash.to_string()),
                from_path: Some(frame.path.to_string()),
            };
            file.titles.push(entry.clone());
            added.push(entry);
        }
    }
    added
}

/// Adds a typed name or title. Un-dismisses it if it had been removed before.
pub fn add_typed(file: &mut LearnedFile, text: &str, now: &str) -> Result<LearnedTitle, String> {
    let key = normalize(text);
    if key.chars().count() < 2 {
        return Err("Type a name or a title to match.".to_string());
    }
    if file.titles.iter().any(|entry| entry.key == key) {
        return Err(format!("“{}” is already on the list.", text.trim()));
    }
    file.dismissed.retain(|dismissed| dismissed != &key);
    let entry = LearnedTitle {
        key,
        title: text.trim().to_string(),
        kind: LearnKind::Term,
        source: "typed".to_string(),
        added_at: now.to_string(),
        from_hash: None,
        from_path: None,
    };
    file.titles.push(entry.clone());
    Ok(entry)
}

/// Removes an entry. One learned from a frame is remembered as dismissed, so the frame that taught
/// it does not teach it again; a typed one simply goes.
pub fn remove(file: &mut LearnedFile, key: &str) -> Option<LearnedTitle> {
    let index = file.titles.iter().position(|entry| entry.key == key)?;
    let entry = file.titles.remove(index);
    if entry.kind == LearnKind::Title && !file.dismissed.iter().any(|dismissed| dismissed == key) {
        file.dismissed.push(key.to_string());
    }
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_of_one_title_normalize_alike() {
        let a = normalize("9 (2) IVE 'OffThe Record' MV");
        assert_eq!(a, "ive offthe record mv");
        assert_eq!(normalize("• (2) IVE 'OffThe Record' MV"), a);
        assert_eq!(normalize("IVE 'OffThe Record' MV - YouTube"), a);
        // Hangul survives as letters.
        assert_eq!(normalize("tripleS(트리플에스) moon"), "triples 트리플에스 moon");
        // A number that IS the title is kept once it is not leading noise.
        assert_eq!(normalize("(2) IVE '8' Lyrics"), "ive 8 lyrics");
    }

    fn learned(title: &str) -> LearnedTitle {
        LearnedTitle {
            key: normalize(title),
            title: title.to_string(),
            kind: LearnKind::Title,
            source: "ocr".into(),
            added_at: String::new(),
            from_hash: None,
            from_path: None,
        }
    }

    #[test]
    fn a_learned_title_matches_its_own_readings_and_not_its_neighbours() {
        let titles = vec![learned("tripleS(E21 moon 'Dream Dress' Special Visual Clip")];
        let matcher = LearnedMatcher::new(&titles);
        assert_eq!(matcher.find(&normalize("tripleS(E21 moon 'Dream Dress' Special Visual Clip")), Some(0));
        // The band lost the artist: a long enough part of the title still identifies it.
        assert_eq!(matcher.find(&normalize("moon 'Dream Dress' Special Visual Clip")), Some(0));
        // A short fragment does not.
        assert_eq!(matcher.find(&normalize("Special Visual Clip")), None);
        assert_eq!(matcher.find(&normalize("Driving in Davao City")), None);
    }

    #[test]
    fn a_typed_term_matches_as_whole_words() {
        let mut file = LearnedFile::default();
        add_typed(&mut file, "Crayon Pop", "now").unwrap();
        let matcher = LearnedMatcher::new(&file.titles);
        assert!(matcher.find(&normalize("[Crayon Pop] 'FM' MN - official")).is_some());
        assert!(matcher.find(&normalize("crayonpopcorn recipe")).is_none());
        assert!(add_typed(&mut file, "crayon  pop", "now").is_err(), "a duplicate is refused");
    }

    #[test]
    fn learning_skips_what_the_rules_know_and_what_was_dismissed() {
        let mut file = LearnedFile::default();
        let frame = |ocr: &'static str, described: &'static str| HandSorted {
            hash: "h",
            path: "p",
            ocr: [ocr, ""],
            described,
        };
        let rules = |title: &str| title.contains("MV");
        let added = learn(
            &mut file,
            [frame("Official Dance (We So Alive Ver.)", ""), frame("IVE 'I AM' MV", ""), frame("", "")],
            rules,
            "now",
        );
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].key, "official dance we so alive ver");

        // Learned once only, even when taught again.
        assert!(learn(&mut file, [frame("Official Dance (We So Alive Ver.)", "")], rules, "now").is_empty());

        // Removed on the panel = not learned again from the same frame.
        remove(&mut file, "official dance we so alive ver");
        assert!(learn(&mut file, [frame("Official Dance (We So Alive Ver.)", "")], rules, "now").is_empty());
        assert!(file.titles.is_empty());
    }
}
