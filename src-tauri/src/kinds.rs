//! Scene-kind classification over the vision descriptions.
//!
//! The geo layer answers *where* a picture is; this answers *whether it shows a place at all*. A
//! shopping-mall interior in Virginia, a K-pop close-up, a talking head over a map — all carry a
//! real country and all resolve happily through the gazetteer, but none of them teach you what
//! anywhere looks like. Measured on a sample of live set members, roughly one in five was this kind
//! of noise.
//!
//! **It runs over the descriptions, not the images.** The vision pass already looked at every
//! picture and wrote prose that says what it is ("a video still of a busy city street at night",
//! "a woman ... in a close-up shot"). Re-reading 10k images through a vision model is hours of GPU;
//! re-reading their prose is a text pass that batches ~20 items per request. The signal is already
//! in the text — this just extracts it.
//!
//! Output is a cached `hash -> kind` sidecar, so the pass runs once and set building filters
//! against it for free. Bump `KIND_PROMPT_VERSION` when the prompt changes so a re-run is
//! distinguishable from a resume.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const KINDS_FILE_NAME: &str = ".image-categorizer-kinds.json";
pub const KIND_SCHEMA_VERSION: u32 = 1;
/// v2 added the reposted-clip and person-in-front-of-a-street rules — see `CLASSIFY_PROMPT`.
/// Bumping this makes the next run clear every stored label and reclassify, which is the point: a
/// library labelled half by one prompt and half by another cannot be reasoned about.
pub const KIND_PROMPT_VERSION: u32 = 2;

/// How many descriptions go into one request. Each is a few hundred characters, so twenty is a
/// comfortable prompt while cutting the request count by 20x versus one-at-a-time.
pub const DEFAULT_BATCH_SIZE: usize = 20;

/// Token ceiling per batch. The *answer* is tiny — one `"12: outdoor"` line per item — but the
/// local models here are reasoning models that spend hundreds to thousands of tokens in a separate
/// `reasoning_content` channel before writing anything. Budgeted at 600 the whole reply came back
/// empty with `finish_reason: length`, which is the same trap `vision.rs` documents. A ceiling only
/// caps runaway generations, so being generous costs nothing on the normal path.
pub const MAX_COMPLETION_TOKENS: u32 = 6000;

pub const KIND_OUTDOOR: &str = "outdoor";
pub const KIND_INDOOR: &str = "indoor";
pub const KIND_PERSON: &str = "person";
pub const KIND_SCREEN: &str = "screen";
pub const KIND_OTHER: &str = "other";

pub const ALL_KINDS: &[&str] = &[KIND_OUTDOOR, KIND_INDOOR, KIND_PERSON, KIND_SCREEN, KIND_OTHER];

/// What a set may draw from unless the library overrides it. Only `outdoor` teaches geography;
/// the rest are the noise this module exists to remove.
pub const DEFAULT_ALLOWED_KINDS: &[&str] = &[KIND_OUTDOOR];

/// Deliberately concrete rather than abstract: local models label far more consistently from
/// examples of what belongs in each bucket than from a definition. The last rule is the important
/// one — nearly every false positive in this library comes from a video *title* naming a country
/// while the frame shows a shop interior or a face.
pub const CLASSIFY_PROMPT: &str = "You label image descriptions by what kind of scene each one shows. \
The goal is to keep only pictures that actually show what a place looks like, for geography practice.

Labels:
outdoor = taken outdoors, showing a real place: street, road, railway line, path, village, town, \
cityscape, landscape, coast, farmland, park, an aerial or drone view, or the view out through the \
windscreen or window of a moving car, train, bus or boat. The surroundings of a real location are visible.
indoor = taken inside a building or vehicle where the outside is not the subject: shop, shopping \
mall, supermarket, restaurant, bar, home, office, factory, museum, arena, station concourse, \
airport terminal, tunnel, or a vehicle cabin looking at the dashboard rather than the road.
person = dominated by one or a few people - portrait, close-up, talking head, interview, stage \
performance, music video - so that the place itself is not really visible.
screen = a screenshot of software, a web page, a video player interface, a search page, a map, a \
chart, a table, a game, a logo, or text and graphics, rather than a photographed place.
other = anything else, or too unclear to tell.

Rules:
- Judge the SCENE THAT IS DESCRIBED, never the video title. A description whose title names a \
country but whose scene is a shop interior is indoor, not outdoor.
- A photographed outdoor place stays outdoor even when a person is in it, as long as the place is \
still visible AND the people are incidental - passers-by, traffic, a crowd in a square.
- If the people are the SUBJECT and the street is merely behind them - posing, a fashion or \
nightlife clip, a music video, an interview, a performance - that is person, not outdoor, however \
much of the street is visible.
- A clip reposted from another platform is screen: a burned-in headline or caption bar across the \
frame, a platform watermark or @handle (TikTok, Instagram, Shorts), or a thumbnail-style title card \
over the picture. These are edited graphics, not a photographed place, even when the footage \
underneath is outdoors.
- A map or satellite view of a country is screen, not outdoor.

Answer with exactly one line per numbered input, formatted \"<number>: <label>\", using only the \
five labels above. No other text.";

// -------------------------------------------------------------------------------------------
// Sidecar
// -------------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindsFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub prompt_version: u32,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub note: String,
    /// Which kinds geo sets may draw from. Editable by hand — set it to
    /// `["outdoor","indoor"]` to let interiors back in, for instance.
    #[serde(default)]
    pub allowed_kinds: Vec<String>,
    /// hash -> one of `ALL_KINDS`, classified from that image's OWN description.
    #[serde(default)]
    pub kinds: BTreeMap<String, String>,
    /// hash -> kind inherited from the other sampled frames of the same video. Kept separate from
    /// `kinds` so a re-run recomputes it wholesale instead of treating a derived guess as evidence.
    #[serde(default)]
    pub propagated: BTreeMap<String, String>,
}

impl KindsFile {
    /// The kind to use for an image: its own classification first, then its video's.
    pub fn kind_for(&self, hash: &str) -> Option<&String> {
        self.kinds.get(hash).or_else(|| self.propagated.get(hash))
    }

    /// Own + inherited as one map, which is what set building filters against.
    pub fn effective(&self) -> BTreeMap<String, String> {
        let mut merged = self.propagated.clone();
        for (hash, kind) in &self.kinds {
            merged.insert(hash.clone(), kind.clone());
        }
        merged
    }
}

/// Fills in the frames a video never had described.
///
/// Only ~10 frames per video get a description (the chunk plan samples for the expensive vision
/// pass), but scene kind is a property of the video far more than of the frame: a mall walkthrough
/// is indoor for all of its frames. This is the same propagation that supplies roughly half of all
/// geo tags, applied to the other axis.
///
/// Plurality wins; a tie propagates nothing, so a genuinely mixed video stays unlabelled and passes
/// through rather than being confidently mislabelled.
pub fn propagate_kinds(
    classified: &BTreeMap<String, String>,
    groups: &[Vec<String>],
) -> BTreeMap<String, String> {
    let mut propagated = BTreeMap::new();
    for members in groups {
        let mut votes: BTreeMap<&str, usize> = BTreeMap::new();
        for hash in members {
            if let Some(kind) = classified.get(hash) {
                *votes.entry(kind.as_str()).or_insert(0) += 1;
            }
        }
        if votes.is_empty() {
            continue;
        }
        let best = votes.values().copied().max().unwrap_or(0);
        let winners: Vec<&str> = votes
            .iter()
            .filter(|(_, count)| **count == best)
            .map(|(kind, _)| *kind)
            .collect();
        if winners.len() != 1 {
            continue; // genuinely mixed — say nothing
        }
        let winner = winners[0].to_string();
        for hash in members {
            if !classified.contains_key(hash) {
                propagated.insert(hash.clone(), winner.clone());
            }
        }
    }
    propagated
}

pub const KINDS_NOTE: &str = "Scene kind per image, derived from its vision description by a \
text-only pass. 'allowedKinds' controls which kinds geo country sets may use - edit it to change \
the filter without reclassifying anything. Delete an entry under 'kinds' to have it reclassified on \
the next run.";

pub fn kinds_path(root: &Path) -> PathBuf {
    root.join(KINDS_FILE_NAME)
}

pub fn load_kinds(root: &Path) -> KindsFile {
    std::fs::read_to_string(kinds_path(root))
        .ok()
        .and_then(|text| serde_json::from_str::<KindsFile>(&text).ok())
        .unwrap_or_default()
}

pub fn save_kinds(root: &Path, file: &KindsFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(file)
        .map_err(|error| format!("Failed to serialize scene kinds: {error}"))?;
    std::fs::write(kinds_path(root), json)
        .map_err(|error| format!("Failed to save scene kinds: {error}"))
}

/// The kinds a set may draw from: the library's own list when it has one, else `outdoor` only.
/// Unknown labels are dropped so a typo in the file cannot silently admit everything.
pub fn allowed_kinds(file: &KindsFile) -> Vec<String> {
    let configured: Vec<String> = file
        .allowed_kinds
        .iter()
        .map(|kind| kind.trim().to_lowercase())
        .filter(|kind| ALL_KINDS.contains(&kind.as_str()))
        .collect();
    if configured.is_empty() {
        DEFAULT_ALLOWED_KINDS.iter().map(|kind| kind.to_string()).collect()
    } else {
        configured
    }
}

// -------------------------------------------------------------------------------------------
// Prompt / response
// -------------------------------------------------------------------------------------------

/// Trims a description to the part that describes the scene, and caps its length.
///
/// The stored prose starts with the video title, which is exactly the thing the model must not
/// judge by — dropping the leading title line removes most of the temptation before the request is
/// even sent, on top of the prompt telling it not to.
pub fn scene_text(description: &str, max_chars: usize) -> String {
    let mut body = description.trim();

    // The title line is written first, separated by a blank line, when the frame came from a video.
    if let Some((first, rest)) = body.split_once("\n\n") {
        let looks_like_title = !first.contains(". ") && first.len() < 200;
        if looks_like_title && !rest.trim().is_empty() {
            body = rest.trim();
        }
    }

    let flattened: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= max_chars {
        return flattened;
    }
    flattened.chars().take(max_chars).collect()
}

/// Builds one batch request: the instructions followed by numbered scene texts.
pub fn build_batch_prompt(scenes: &[String]) -> String {
    let mut prompt = String::with_capacity(CLASSIFY_PROMPT.len() + scenes.len() * 400);
    prompt.push_str(CLASSIFY_PROMPT);
    prompt.push_str("\n\n");
    for (index, scene) in scenes.iter().enumerate() {
        prompt.push_str(&format!("{}: {}\n", index + 1, scene));
    }
    prompt
}

/// Parses `"<n>: <label>"` lines back into a slot per input.
///
/// Tolerant on purpose — models add stray bullets, quotes or a "Here are the labels:" preamble, and
/// throwing the whole batch away over formatting would waste twenty classifications. Anything
/// unmatched stays `None` and is simply retried on a later run.
pub fn parse_batch_response(text: &str, expected: usize) -> Vec<Option<String>> {
    let mut out = vec![None; expected];
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '#', ' ']);
        // Models number their lines as "1:", "1." or "1)" more or less at random.
        let Some(split_at) = line.find([':', '.', ')']) else {
            continue;
        };
        let (number, label) = line.split_at(split_at);
        let label = &label[1..];
        let number: String = number.trim().chars().filter(|c| c.is_ascii_digit()).collect();
        let Ok(index) = number.parse::<usize>() else {
            continue;
        };
        if index == 0 || index > expected {
            continue;
        }
        let label = label
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c == ',')
            .to_lowercase();
        // Accept a bare label or one with trailing commentary ("outdoor - a street scene").
        let head = label.split_whitespace().next().unwrap_or("");
        if let Some(kind) = ALL_KINDS.iter().find(|kind| **kind == head) {
            out[index - 1] = Some((*kind).to_string());
        }
    }
    out
}

/// Classifies one batch of scene texts. Returns a slot per input; `None` means the model's reply
/// did not cover that item.
pub fn classify_batch(
    agent: &ureq::Agent,
    endpoint: &str,
    model: &str,
    api_key: Option<&str>,
    scenes: &[String],
) -> Result<Vec<Option<String>>, String> {
    if scenes.is_empty() {
        return Ok(Vec::new());
    }
    let prompt = build_batch_prompt(scenes);
    let reply = crate::vision::chat_text(agent, endpoint, model, api_key, &prompt, MAX_COMPLETION_TOKENS)?;
    Ok(parse_batch_response(&reply, scenes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_leading_video_title() {
        let described = "Cab Ride Belo Horizonte - YouTube\n\nThis is a video still showing the \
                         perspective of a train driver. Location: Brazil";
        let scene = scene_text(described, 400);
        assert!(scene.starts_with("This is a video still"), "got: {scene}");
        assert!(!scene.contains("Cab Ride"));
    }

    #[test]
    fn keeps_prose_that_has_no_title_line() {
        let described = "This is a professional photo portrait of Barack Obama. He is looking \
                         directly at the camera.";
        let scene = scene_text(described, 400);
        assert!(scene.starts_with("This is a professional photo portrait"));
    }

    #[test]
    fn parses_well_formed_replies() {
        let parsed = parse_batch_response("1: outdoor\n2: indoor\n3: person", 3);
        assert_eq!(
            parsed,
            vec![
                Some("outdoor".into()),
                Some("indoor".into()),
                Some("person".into())
            ]
        );
    }

    #[test]
    fn tolerates_the_junk_models_add() {
        // Preamble, bullets, quotes, trailing commentary, and a stray out-of-range index: none of
        // these may cost the whole batch.
        let reply = "Here are the labels:\n- 1: \"outdoor\"\n* 2: screen - a map view\n3. indoor\n\
                     9: outdoor\nnonsense line";
        let parsed = parse_batch_response(reply, 3);
        assert_eq!(parsed[0], Some("outdoor".into()));
        assert_eq!(parsed[1], Some("screen".into()));
        assert_eq!(parsed[2], Some("indoor".into()));
    }

    #[test]
    fn unmatched_items_stay_none_for_a_later_retry() {
        let parsed = parse_batch_response("1: outdoor", 3);
        assert_eq!(parsed[0], Some("outdoor".into()));
        assert_eq!(parsed[1], None);
        assert_eq!(parsed[2], None);
    }

    #[test]
    fn unknown_labels_are_rejected_rather_than_guessed() {
        let parsed = parse_batch_response("1: streetscape\n2: outdoor", 2);
        assert_eq!(parsed[0], None);
        assert_eq!(parsed[1], Some("outdoor".into()));
    }

    #[test]
    fn propagates_a_videos_kind_to_its_undescribed_frames() {
        let mut classified = BTreeMap::new();
        classified.insert("a1".to_string(), "indoor".to_string());
        classified.insert("a2".to_string(), "indoor".to_string());
        classified.insert("a3".to_string(), "outdoor".to_string());
        let groups = vec![vec![
            "a1".to_string(),
            "a2".to_string(),
            "a3".to_string(),
            "a4".to_string(),
            "a5".to_string(),
        ]];
        let propagated = propagate_kinds(&classified, &groups);
        // Plurality (2 indoor vs 1 outdoor) reaches only the undescribed frames.
        assert_eq!(propagated.get("a4"), Some(&"indoor".to_string()));
        assert_eq!(propagated.get("a5"), Some(&"indoor".to_string()));
        assert!(!propagated.contains_key("a1"), "classified frames keep their own label");
        assert!(!propagated.contains_key("a3"));
    }

    #[test]
    fn a_tied_video_propagates_nothing() {
        let mut classified = BTreeMap::new();
        classified.insert("b1".to_string(), "indoor".to_string());
        classified.insert("b2".to_string(), "outdoor".to_string());
        let groups = vec![vec!["b1".to_string(), "b2".to_string(), "b3".to_string()]];
        // A genuinely mixed video must stay unlabelled and pass through, not be guessed at.
        assert!(propagate_kinds(&classified, &groups).is_empty());
    }

    #[test]
    fn own_classification_beats_an_inherited_one() {
        let mut file = KindsFile::default();
        file.kinds.insert("x".into(), "outdoor".into());
        file.propagated.insert("x".into(), "indoor".into());
        file.propagated.insert("y".into(), "indoor".into());
        assert_eq!(file.kind_for("x"), Some(&"outdoor".to_string()));
        assert_eq!(file.kind_for("y"), Some(&"indoor".to_string()));
        assert_eq!(file.effective().get("x"), Some(&"outdoor".to_string()));
    }

    #[test]
    fn allowed_kinds_defaults_to_outdoor_and_ignores_typos() {
        let mut file = KindsFile::default();
        assert_eq!(allowed_kinds(&file), vec!["outdoor".to_string()]);
        file.allowed_kinds = vec!["nonsense".into()];
        assert_eq!(allowed_kinds(&file), vec!["outdoor".to_string()]);
        file.allowed_kinds = vec!["outdoor".into(), "INDOOR".into()];
        assert_eq!(
            allowed_kinds(&file),
            vec!["outdoor".to_string(), "indoor".to_string()]
        );
    }
}
