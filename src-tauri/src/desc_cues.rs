//! What each saved vision description says about the window title — a second, better reading of
//! the same title band the OCR title pass reads.
//!
//! The Describe prompt asks the model to quote the window title first (`vision::DESCRIBE_PROMPT`),
//! and it reads that band far better than Windows OCR: `tripleS(트리플에스) moon 'Dream Dress'
//! Special Visual Clip - YouTube` where OCR has `tripleS(E21 moon …`, and `NMIXX(엔믹스) '별별별 (See
//! that?)' M/V` where OCR kept only `e e e (See that?)" M/V`. Measured 2026-10-20 on the live
//! library: 590 Low Text frames whose description names a K-pop act or format, 427 of them frames
//! the OCR pass had stored as untitled.
//!
//! This is derived data, never a decision: the K-pop rule (`kpop.rs`) is run over it on every
//! save, exactly as it is over `video_title`. Nothing here calls a model — it reads the
//! `<hash>.txt` files the Describe pass already wrote.
//!
//! The cache file exists because reading ~50,000 description files costs a second or two, and the
//! rule runs on every checkpoint save. One entry per described image; an entry whose `chars` no
//! longer matches the record's `vision_desc_chars` was re-described and is read again.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub const CUES_FILE_NAME: &str = ".image-categorizer-desc-cues.json";

/// Bump when [`parse`] changes what it extracts: every stored entry is then discarded and re-read,
/// because two parsers' output in one file would be indistinguishable afterwards.
const CUES_VERSION: u32 = 1;

/// How much of the description proper is kept. The K-pop rule reads only the opening ("This is a
/// video still from a music video featuring…"); further down the model lists thumbnails and
/// on-screen text, which describe what is AROUND the picture.
const LEAD_MAX_CHARS: usize = 240;
const TITLE_MAX_CHARS: usize = 300;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    /// Length of the description this was read from. A forced re-describe changes it.
    #[serde(rename = "c")]
    pub chars: u32,
    /// The title line as the model quoted it, minus any "Title-bar text:" label. Empty when the
    /// description opens straight with the scene.
    #[serde(rename = "t", default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// The title line lists several tabs or windows rather than one title — Vivaldi with its tab
    /// strip showing. Such a line says what was OPEN, not what is on screen.
    #[serde(rename = "m", default, skip_serializing_if = "is_false")]
    pub tab_strip: bool,
    /// The first sentences of the description proper.
    #[serde(rename = "l", default, skip_serializing_if = "String::is_empty")]
    pub lead: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub type CueMap = HashMap<String, Cue>;

#[derive(Default, Serialize, Deserialize)]
struct CueFile {
    version: u32,
    #[serde(default)]
    entries: CueMap,
}

pub fn cues_path(root: &Path) -> PathBuf {
    root.join(CUES_FILE_NAME)
}

fn body_opener() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(?:this\s+(?:is|image|screenshot|photo|picture|video|still|collage|composite)|(?:a|an)\s+(?:screenshot|photo|photograph|picture|image|video|still|close-up|closeup|digital|computer|aerial|illustration|collage|composite|view|wide|dark|black|blurry|first-person|cropped|desktop|web)|image of|video still|screenshot of|location\s*:)",
        )
        .expect("body opener regex")
    })
}

/// A first line that is prose, not a title: "A promotional photo of a six-member girl group …". Video
/// titles are written in title case, so an article followed by a lowercase word, ending in a full
/// stop, is the model starting its description without quoting a title first.
fn sentence_like() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(?:A|An|The|This)\s+[a-z][^\n]*\.$").expect("sentence regex"))
}

fn title_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(?:visible\s+)?(?:title[- ]?bar(?:\s+text)?|window\s+titles?|tab\s+titles?|titles?|browser\s+tabs?|tabs?)\s*[:：]\s*",
        )
        .expect("title label regex")
    })
}

fn quoted_segment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""[^"]{2,}"|`[^`]{2,}`|“[^”]{2,}”"#).expect("quoted segment regex"))
}

/// Tab names the model lists as their own entries, which only ever happens in a tab strip.
const LONE_TAB_NAMES: &[&str] = &["youtube", "new tab", "new chat", "history", "google maps", "settings", "notes"];

/// Reads the title line and the opening of one description.
pub fn parse(description: &str) -> Cue {
    let chars = description.chars().count() as u32;
    let lines: Vec<&str> = description.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let Some(first) = lines.first() else {
        return Cue { chars, ..Cue::default() };
    };

    let (title, body_from) = if body_opener().is_match(first) || sentence_like().is_match(first) {
        (String::new(), 0)
    } else {
        (title_label().replace(first, "").trim().to_string(), 1)
    };
    // A bare address bar says nothing about what the video is; as a "title" it would count as
    // evidence that the session had moved on from a K-pop run.
    let title = if title.starts_with("http") && !title.contains(' ') { String::new() } else { title };
    let tab_strip = !title.is_empty() && is_tab_strip(&title);

    let body: Vec<&str> = lines[body_from.min(lines.len())..]
        .iter()
        .copied()
        .filter(|line| !line.to_ascii_lowercase().starts_with("location:"))
        .collect();
    Cue {
        chars,
        title: truncate_chars(&strip_wrapping(&title), TITLE_MAX_CHARS),
        tab_strip,
        lead: lead_of(&body.join(" ")),
    }
}

/// Several titles on one line: two or more quoted segments, a truncated tab name ("The Consumerism
/// Slop Epic…"), or a bare tab name like "YouTube" among separated segments.
///
/// NOT a `•` or `·` by itself: video titles use them too — "Kaunas in May 2025 • Drive • LT" is one
/// title, and taking it for a tab strip let a driving video be filled into K-pop between two MVs.
fn is_tab_strip(line: &str) -> bool {
    if quoted_segment().find_iter(line).count() >= 2 {
        return true;
    }
    if line.contains("...") || line.contains('…') {
        return true;
    }
    let segments = split_segments(line);
    // A tab's name runs on after the site ("New chat - Claud…"), so a prefix counts too.
    segments.len() >= 2
        && segments.iter().any(|segment| {
            let segment = segment.to_lowercase();
            LONE_TAB_NAMES
                .iter()
                .any(|name| segment == *name || segment.starts_with(&format!("{name} ")))
        })
}

fn notification_count() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(\d+\)\s*").expect("notification count regex"))
}

/// A line cut at the separators the model writes between tabs, each piece stripped of its quotes
/// and of a leading notification count ("(2) YouTube").
fn split_segments(line: &str) -> Vec<String> {
    let quoted: Vec<&str> = quoted_segment().find_iter(line).map(|found| found.as_str()).collect();
    let pieces: Vec<&str> = if quoted.len() >= 2 {
        quoted
    } else {
        line.split([',', ';', '·', '•']).flat_map(|piece| piece.split("...")).flat_map(|piece| piece.split('…')).collect()
    };
    pieces
        .into_iter()
        .map(|piece| piece.trim_matches(|c: char| matches!(c, '"' | '`' | '\'' | '“' | '”') || c.is_whitespace()))
        .map(|piece| notification_count().replace(piece, "").trim().to_string())
        .filter(|piece| !piece.is_empty())
        .collect()
}

/// The tabs of a tab strip that could be what is on screen. An audio player's tab is dropped: it
/// names what is PLAYING, and measured on this library, a "Distance - Gfriend - Deezer" tab sat
/// beside a Scooter music video and "FM - Crayon Pop - Deezer" beside 48 frames of a motorcycle
/// video — the song was the soundtrack to watching something else.
pub fn visible_tabs(title: &str) -> String {
    split_segments(title)
        .into_iter()
        .filter(|segment| {
            let lower = segment.to_lowercase();
            !AUDIO_PLAYERS.iter().any(|player| lower.ends_with(player) || lower.contains(&format!("{player} ")))
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

const AUDIO_PLAYERS: &[&str] = &["deezer", "spotify", "soundcloud", "apple music", "youtube music"];

/// Drops the quotes, backticks or brackets the model wraps a single title in.
fn strip_wrapping(title: &str) -> String {
    let trimmed = title.trim();
    let pairs = [('"', '"'), ('`', '`'), ('“', '”'), ('[', ']'), ('\'', '\'')];
    for (open, close) in pairs {
        if trimmed.len() >= 2 && trimmed.starts_with(open) && trimmed.ends_with(close) {
            let inner = &trimmed[open.len_utf8()..trimmed.len() - close.len_utf8()];
            // Only a wrapper if it does not also occur inside — `"A" and "B"` is two quotes.
            if !inner.contains(open) && !inner.contains(close) {
                return inner.trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

/// The first sentences of the body, up to [`LEAD_MAX_CHARS`]: at least one, and a second when the
/// first is short ("This is a video still." says nothing on its own).
fn lead_of(body: &str) -> String {
    let mut lead = String::new();
    for sentence in body.split_inclusive(". ") {
        lead.push_str(sentence);
        if lead.chars().count() >= 80 {
            break;
        }
    }
    truncate_chars(lead.trim(), LEAD_MAX_CHARS)
}

fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((index, _)) => text[..index].to_string(),
        None => text.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// The cache: one file per library, held in memory per process
// ---------------------------------------------------------------------------------------------

struct Cached {
    /// The file's (mtime, length) when it was last read or written here, so a write by another
    /// process (the nightly run) is noticed without parsing the file to find out.
    stamp: Option<(SystemTime, u64)>,
    map: Arc<CueMap>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn read_file(path: &Path) -> CueMap {
    fs::read_to_string(path)
        .ok()
        .and_then(|data| serde_json::from_str::<CueFile>(&data).ok())
        .filter(|file| file.version == CUES_VERSION)
        .map(|file| file.entries)
        .unwrap_or_default()
}

/// The cues as last known for `root` — read from disk only when the file changed since this
/// process last looked. Entries this process read but has not persisted yet survive a reload:
/// dropping them would flip their frames out of K-pop and back again one save later.
pub fn shared(root: &Path) -> Arc<CueMap> {
    let path = cues_path(root);
    let stamp = file_stamp(&path);
    let Ok(mut cache) = cache().lock() else {
        return Arc::new(read_file(&path));
    };
    if let Some(cached) = cache.get(&path) {
        if cached.stamp == stamp {
            return Arc::clone(&cached.map);
        }
    }
    let mut map = read_file(&path);
    if let Some(previous) = cache.get(&path) {
        for (hash, cue) in previous.map.iter() {
            map.entry(hash.clone()).or_insert_with(|| cue.clone());
        }
    }
    let map = Arc::new(map);
    cache.insert(path, Cached { stamp, map: Arc::clone(&map) });
    map
}

/// Brings the cues up to date with the descriptions on disk: reads each described image that has
/// no entry, or whose entry was parsed from a description of a different length. `described` is
/// every `(hash, vision_desc_chars)` the library holds.
///
/// Cheap when nothing changed — a lookup per record, no file reads, no clone. `persist` writes the
/// cache file when something was read; the per-image commits of a Describe run pass false (a 10 MB
/// write per image would cost more than the image), and the next scan persists what they added.
pub fn refresh<'a>(root: &Path, described: impl IntoIterator<Item = (&'a str, u32)>, persist: bool) -> Arc<CueMap> {
    let current = shared(root);
    let stale: Vec<(&str, u32)> = described
        .into_iter()
        .filter(|(hash, chars)| current.get(*hash).map_or(true, |cue| cue.chars != *chars))
        .collect();
    if stale.is_empty() {
        return current;
    }

    let desc_dir = root.join(crate::VISION_DESC_DIR_NAME);
    let mut map: CueMap = (*current).clone();
    for (hash, chars) in stale {
        // A record that claims a description with no file behind it gets an empty entry, so it is
        // not looked for again on every save.
        let mut cue = read_description(&desc_dir, hash).map(|text| parse(&text)).unwrap_or_default();
        // Keyed to the RECORD's count, whatever the file holds — it is what the next refresh
        // compares against, and a mismatch would have this file re-read on every save.
        cue.chars = chars;
        map.insert(hash.to_string(), cue);
    }
    let map = Arc::new(map);
    let path = cues_path(root);
    let mut stamp = file_stamp(&path);
    if persist {
        let file = CueFile { version: CUES_VERSION, entries: (*map).clone() };
        if let Ok(data) = serde_json::to_string(&file) {
            if fs::write(&path, data).is_ok() {
                stamp = file_stamp(&path);
            }
        }
    }
    if let Ok(mut cache) = cache().lock() {
        cache.insert(path, Cached { stamp, map: Arc::clone(&map) });
    }
    map
}

/// The prose of one description: the `.txt` sidecar, which holds exactly the `description` field
/// of the `.json` beside it and needs no parsing.
fn read_description(desc_dir: &Path, hash: &str) -> Option<String> {
    if let Ok(text) = fs::read_to_string(desc_dir.join(format!("{hash}.txt"))) {
        return Some(text);
    }
    let json = fs::read_to_string(desc_dir.join(format!("{hash}.json"))).ok()?;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    value.get("description")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_quoted_title_is_unwrapped() {
        let cue = parse("\"NMIXX(엔믹스) '별별별 (See that?)' M/V - YouTube\"\n\nThis is a video still from a music video. The image features a close-up of a woman.");
        assert_eq!(cue.title, "NMIXX(엔믹스) '별별별 (See that?)' M/V - YouTube");
        assert!(!cue.tab_strip);
        assert!(cue.lead.starts_with("This is a video still from a music video."));
    }

    #[test]
    fn a_tab_strip_is_recognised_in_each_shape_the_model_writes() {
        for line in [
            "\"FM - Crayon Pop - Deezer\", \"YouTube\", and \"The Consumerism Slop Epic...\"",
            "FM - Crayon Pop - Deezer YouTube The Consumerism Slop Epis...",
            "FM - Crayon Pop - Deezer · YouTube · The Consumerism Slop Epic...",
            "Title-bar text: `FM - Crayon Pop - Deezer`, `YouTube`, `The Consumerism Slop Epic...`",
            "FM - Crayon Pop, Momentum - sh, New chat - Claud, Kauppakeskus Es",
            "FM - Crayon Pop - Deezer, YouTube",
        ] {
            let cue = parse(&format!("{line}\n\nThis is a screenshot of a YouTube video."));
            assert!(cue.tab_strip, "should be a tab strip: {line:?}");
            assert!(!cue.title.starts_with("Title-bar"), "label kept: {:?}", cue.title);
        }
        // A title with commas or bullets in it is still one title.
        for line in ["Driving in Davao City, Philippines | 4K - YouTube", "(2) Kaunas in May 2025 • Drive • LT - YouTube"] {
            let single = parse(&format!("{line}\n\nThis is a video still."));
            assert!(!single.tab_strip, "one title: {line:?}");
        }
        // A tab name with a notification count is still a tab name.
        assert!(parse("(2) YouTube, (2) Hearts2Hearts 하즈투하츠 - S, Brisbane, AUSTRALIA - S\n\nThis is a video still.").tab_strip);
    }

    #[test]
    fn audio_player_tabs_are_not_what_is_on_screen() {
        let visible = visible_tabs("\"YouTube\", \"Scooter - Megamix Final 2\", \"Distance - Gfriend - Deezer\"");
        assert!(visible.contains("Scooter"));
        assert!(!visible.contains("Gfriend"));
        assert!(!visible_tabs("FM - Crayon Pop - Deezer, YouTube, The Consumerism Slop Epic...").contains("Crayon"));
        assert!(visible_tabs("\"Billlie 'EUNOIA' M/V - You...\", \"New tab\"").contains("EUNOIA"));
    }

    #[test]
    fn a_description_without_a_title_line_has_only_a_lead() {
        let cue = parse("This is an album cover photo for the K-pop group Crayon Pop. Five women pose in a quarry.");
        assert!(cue.title.is_empty());
        assert!(cue.lead.contains("K-pop group Crayon Pop"));
        let located = parse("Location: Espoo\n\nThis is a screenshot of a desktop.");
        assert!(located.title.is_empty());
        assert_eq!(located.lead, "This is a screenshot of a desktop.");
    }

    #[test]
    fn the_lead_stops_after_the_opening_sentences() {
        let long = "x. ".repeat(10) + &"This is a long sentence about the picture itself. ".repeat(10);
        let cue = parse(&format!("\"Title\"\n\n{long}"));
        assert!(cue.lead.chars().count() <= LEAD_MAX_CHARS);
    }
}
