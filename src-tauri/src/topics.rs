//! Phase 2 of the text layer: what a stretch of screenshots was *about*, named by the local model.
//!
//! The premise this exists to serve, in the user's own framing: a screenshot is an intentionally
//! fragmented, valued-in-the-moment snippet, and what it records is **what they thought was
//! important — topics, concepts and keywords, not conclusions**. So this deliberately does not
//! summarize. It names subjects, and the names become keys for other lookups.
//!
//! **Per bucket, not per image, and that is the whole cost story.** Statistical keywords already
//! exist for every document for free (tf-idf off the search index's own postings, no model). What
//! they cannot do is recognise that `webview2`, `cdp` and `9400` are one subject. That is a
//! synthesis job, and doing it per image would be ~8,800 model calls; doing it per 48-hour bucket
//! is **44 across this entire library**, growing about fifteen a month. Text in, text out — no
//! vision, so it is cheap even on a busy day.
//!
//! Nothing here runs by itself. Like every other model pass in this app it is a button and a CLI
//! verb, never a side effect of opening a panel.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::text_index::{self, Bucket};

pub const TOPICS_FILE_NAME: &str = ".image-categorizer-text-topics.json";
pub const TOPICS_SCHEMA_VERSION: u32 = 1;

/// Bumping this invalidates every stored bucket, exactly as `kinds.rs` does for scene labels:
/// keeping answers written against an older rubric would mix two rubrics in one file and there
/// would be no way to tell which is which afterwards.
pub const TOPIC_PROMPT_VERSION: u32 = 1;

/// How much OCR text one bucket's prompt may carry. ~3k tokens — small enough that a bucket costs
/// seconds, large enough to span a working day's subjects. Sampling (below) is what makes this
/// budget representative rather than merely truncated.
pub const DEFAULT_PROMPT_BUDGET: usize = 12_000;

/// Per sampled document. Enough for a window title plus the first lines of content, which is where
/// a screenshot's subject almost always is.
const EXCERPT_CHARS: usize = 220;

const MAX_TOPICS: usize = 8;
const MAX_NOTABLE: usize = 10;

/// Completion ceiling. **Measured, because the obvious value silently fails.**
///
/// The answer itself is ~150 tokens, so 700 looked generous — and every bucket came back empty with
/// `finish_reason: length`. The standard local model here (`google/gemma-4-26b-a4b`) is a reasoning
/// model: it spends the budget thinking and emits `content` only at the end, so a ceiling that is
/// merely larger than the answer produces nothing at all rather than a truncated answer.
///
/// Measured 2026-08-15 against the live server on a full-size prompt: 3,886 prompt tokens in,
/// **1,752 completion tokens** out, of which 5,457 characters were `reasoning_content`. 3,000 is
/// that with room to spare. Raising it costs nothing when unused — only generated tokens are paid
/// for — so this errs high on purpose.
const MAX_TOKENS: u32 = 3_000;

/// How much room a second attempt gets. How long a reasoning model thinks varies per bucket, not
/// just per prompt size: at 3,000 two buckets in three answered and the third still ran out. Rather
/// than raise the ceiling for everyone to cover the worst case, the rare long thinker gets one
/// retry. Failing twice is then a real failure and is reported as one.
const RETRY_MAX_TOKENS: u32 = 8_000;

pub const TOPICS_NOTE: &str = "Written by the Extracted Text topic pass (icat topics). Bucket keys \
are time-bucket labels at the stated width; `fingerprint` is over the images the bucket held when \
it was written, so a bucket that gained images reads as stale rather than silently wrong.";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BucketTopics {
    pub topics: Vec<String>,
    pub notable: Vec<String>,
    pub docs: usize,
    pub sampled: usize,
    /// Over the member hashes present when this was written. Cheap staleness: a bucket that gained
    /// or lost an image no longer matches, so a re-run can skip everything that did not move.
    pub fingerprint: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicsFile {
    pub version: u32,
    pub prompt_version: u32,
    pub generated_at: String,
    pub model: String,
    pub note: String,
    /// Width key (`"48h"`) -> bucket label -> topics. Keyed by width because a bucket label only
    /// means anything at the width that produced it; running a second width adds an entry rather
    /// than overwriting the first, so zoom levels do not fight.
    pub widths: BTreeMap<String, BTreeMap<String, BucketTopics>>,
}

impl Default for TopicsFile {
    fn default() -> Self {
        Self {
            version: TOPICS_SCHEMA_VERSION,
            prompt_version: TOPIC_PROMPT_VERSION,
            generated_at: String::new(),
            model: String::new(),
            note: TOPICS_NOTE.to_string(),
            widths: BTreeMap::new(),
        }
    }
}

pub fn width_key(width_hours: u32) -> String {
    format!("{width_hours}h")
}

pub fn topics_path(root: &Path) -> PathBuf {
    root.join(TOPICS_FILE_NAME)
}

pub fn load(root: &Path) -> TopicsFile {
    let file: Option<TopicsFile> = fs::read_to_string(topics_path(root))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok());

    match file {
        // A schema or prompt change makes the stored answers unreadable or unmixable; either way
        // the honest move is to start empty rather than to serve them alongside new ones.
        Some(file) if file.version == TOPICS_SCHEMA_VERSION && file.prompt_version == TOPIC_PROMPT_VERSION => file,
        _ => TopicsFile::default(),
    }
}

pub fn save(root: &Path, file: &TopicsFile) -> Result<(), String> {
    let data = serde_json::to_string_pretty(file)
        .map_err(|error| format!("Failed to serialize topics: {error}"))?;
    fs::write(topics_path(root), data).map_err(|error| format!("Failed to save topics: {error}"))
}

/// Stable over the SET of member hashes — order-independent, because bucket membership is a set and
/// the order `timeline` happens to emit is not part of the meaning.
pub fn fingerprint(hashes: &[String]) -> String {
    let mut sorted: Vec<&str> = hashes.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for value in sorted {
        for byte in value.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Copies stored topics onto the buckets a timeline just produced, marking any whose membership has
/// moved since they were written.
pub fn apply(buckets: &mut [Bucket], file: &TopicsFile, width_hours: u32) {
    let Some(stored) = file.widths.get(&width_key(width_hours)) else {
        return;
    };
    for bucket in buckets.iter_mut() {
        let Some(entry) = stored.get(&bucket.id) else {
            continue;
        };
        bucket.topics = entry.topics.clone();
        bucket.notable = entry.notable.clone();
        bucket.topics_stale = entry.fingerprint != fingerprint(&bucket.hashes);
    }
}

/// A bucket that still needs the model, in the order it should be asked about. `force` re-runs
/// everything; otherwise a bucket is skipped when its membership fingerprint still matches.
pub fn pending<'a>(buckets: &'a [Bucket], file: &TopicsFile, width_hours: u32, force: bool) -> Vec<&'a Bucket> {
    let stored = file.widths.get(&width_key(width_hours));
    buckets
        .iter()
        .filter(|bucket| bucket.images > 0)
        .filter(|bucket| {
            if force {
                return true;
            }
            match stored.and_then(|map| map.get(&bucket.id)) {
                Some(entry) => entry.fingerprint != fingerprint(&bucket.hashes),
                None => true,
            }
        })
        .collect()
}

/// Picks which of a bucket's documents the model sees, spread EVENLY ACROSS THE BUCKET IN TIME.
///
/// Taking the first N would hand a two-day span whatever happened before lunch on the first day; a
/// single busy hour routinely holds more captures than the rest of the bucket together. Even
/// spacing costs nothing and is the difference between "what were the subjects" and "what was the
/// subject that morning".
pub fn sample_evenly<T: Clone>(items: &[T], count: usize) -> Vec<T> {
    if items.is_empty() || count == 0 {
        return Vec::new();
    }
    if items.len() <= count {
        return items.to_vec();
    }
    (0..count)
        .map(|position| {
            let index = (position * items.len()) / count;
            items[index.min(items.len() - 1)].clone()
        })
        .collect()
}

/// One document as the prompt sees it.
#[derive(Debug, Clone)]
pub struct TopicSource {
    pub at: String,
    pub terms: Vec<String>,
    pub excerpt: String,
}

/// Trims OCR text down to a usable excerpt: blank lines dropped, whitespace collapsed, cut at a
/// character boundary. OCR output is one recognized line per line and is full of one-character
/// noise rows, which would otherwise eat the budget.
pub fn excerpt(text: &str, limit: usize) -> String {
    let mut out = String::with_capacity(limit.min(text.len()) + 8);
    for line in text.lines() {
        let line = line.trim();
        if line.chars().count() < 2 {
            continue;
        }
        if !out.is_empty() {
            out.push_str(" · ");
        }
        out.push_str(line);
        if out.chars().count() >= limit {
            break;
        }
    }
    out.chars().take(limit).collect()
}

/// The instruction. Deliberately asks for subjects and named things, and explicitly NOT for a
/// summary or a conclusion — a screenshot trail records what looked important at the time, and
/// flattening it into prose destroys the thing that makes it a usable key later.
pub fn build_prompt(label: &str, terms: &[String], sources: &[TopicSource]) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "Below are fragments of text read by OCR from screenshots one person took during a single \
         stretch of time. They chose to capture each one because something on screen mattered to \
         them right then.\n\n\
         Name what they were working with. NOT a summary, NOT what they concluded — the recurring \
         subjects, tools, projects and concepts.\n\n\
         The OCR is noisy: words are mangled, several applications often appear in one capture, and \
         some fragments are unreadable. Ignore garbled runs, window chrome, menu labels, clocks and \
         anything that is merely the operating system.\n\n",
    );
    prompt.push_str("Reply with ONLY a JSON object, no prose and no code fence:\n");
    prompt.push_str(
        "{\"topics\": [\"...\"], \"notable\": [\"...\"]}\n\n\
         topics: 3 to 8 short noun phrases naming the themes, most prominent first. Lowercase \
         unless a proper noun.\n\
         notable: up to 10 specific named things spelled as they appear on screen — tools, \
         libraries, projects, file names, error strings, people, places. Omit anything you are \
         guessing at.\n\n",
    );

    prompt.push_str(&format!("Time span: {label}\n"));
    if !terms.is_empty() {
        prompt.push_str(&format!("Frequent distinctive words: {}\n", terms.join(", ")));
    }
    prompt.push_str("\nFragments:\n");

    for source in sources {
        prompt.push_str(&format!("[{}] {}", source.at, source.excerpt));
        if !source.terms.is_empty() {
            prompt.push_str(&format!("  (keywords: {})", source.terms.join(", ")));
        }
        prompt.push('\n');
    }
    prompt
}

/// Pulls the answer out of whatever the model wrapped it in. Local models fence JSON, prepend "Here
/// is", or add a trailing note however firmly the prompt asks them not to, so the outermost
/// brace-delimited span is taken rather than trusting the whole reply to parse.
pub fn parse_reply(reply: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let start = reply.find('{').ok_or_else(|| "reply contained no JSON object".to_string())?;
    let end = reply.rfind('}').ok_or_else(|| "reply contained no JSON object".to_string())?;
    if end <= start {
        return Err("reply contained no JSON object".to_string());
    }
    let value: serde_json::Value = serde_json::from_str(&reply[start..=end])
        .map_err(|error| format!("reply was not valid JSON: {error}"))?;

    let list = |key: &str, cap: usize| -> Vec<String> {
        value[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .map(|item| item.trim())
                    .filter(|item| !item.is_empty())
                    .map(str::to_string)
                    .take(cap)
                    .collect()
            })
            .unwrap_or_default()
    };

    let topics = list("topics", MAX_TOPICS);
    let notable = list("notable", MAX_NOTABLE);
    if topics.is_empty() && notable.is_empty() {
        return Err("reply had neither topics nor notable entries".to_string());
    }
    Ok((topics, notable))
}

/// Everything the caller needs to ask the model about one bucket. Assembled here so the app's
/// background thread and the CLI build identical prompts.
pub fn prepare(
    bucket: &Bucket,
    read_text: impl Fn(&str) -> Option<String>,
    doc_terms: impl Fn(&str) -> Vec<String>,
    doc_at: impl Fn(&str) -> String,
    budget: usize,
) -> (String, usize) {
    let slots = (budget / (EXCERPT_CHARS + 40)).max(1);
    let sampled = sample_evenly(&bucket.hashes, slots);

    let mut sources: Vec<TopicSource> = Vec::new();
    let mut used = 0usize;
    for hash in &sampled {
        let Some(text) = read_text(hash) else { continue };
        let body = excerpt(&text, EXCERPT_CHARS);
        if body.is_empty() {
            continue;
        }
        used += body.chars().count();
        sources.push(TopicSource {
            at: doc_at(hash),
            terms: doc_terms(hash),
            excerpt: body,
        });
        if used >= budget {
            break;
        }
    }

    let count = sources.len();
    (build_prompt(&bucket.id, &bucket.terms, &sources), count)
}

/// Whether an error is the reasoning model running out of room rather than a real failure.
///
/// Matched on the message because that is what `vision::chat_text` returns, and it is the one place
/// that string is constructed — the alternative is threading a `finish_reason` through a signature
/// three other passes share for no other reason.
pub fn is_out_of_room(error: &str) -> bool {
    error.contains("finish_reason: length")
}

/// Asks the model about one bucket, retrying once with more room if it thought itself out of budget.
/// Shared by the panel's background pass and the CLI so both behave identically — a retry policy
/// that exists in one of them only is how the two surfaces start disagreeing about what "failed"
/// means.
pub fn ask(
    agent: &ureq::Agent,
    endpoint: &str,
    model: &str,
    api_key: Option<&str>,
    prompt: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let first = crate::vision::chat_text(agent, endpoint, model, api_key, prompt, MAX_TOKENS);
    let reply = match first {
        Ok(reply) => reply,
        Err(error) if is_out_of_room(&error) => {
            crate::vision::chat_text(agent, endpoint, model, api_key, prompt, RETRY_MAX_TOKENS)?
        }
        Err(error) => return Err(error),
    };
    parse_reply(&reply)
}

/// Records one bucket's answer.
#[allow(clippy::too_many_arguments)]
pub fn record(
    file: &mut TopicsFile,
    width_hours: u32,
    bucket: &Bucket,
    topics: Vec<String>,
    notable: Vec<String>,
    sampled: usize,
    model: &str,
    now_iso: &str,
) {
    file.version = TOPICS_SCHEMA_VERSION;
    file.prompt_version = TOPIC_PROMPT_VERSION;
    file.generated_at = now_iso.to_string();
    file.model = model.to_string();
    file.note = TOPICS_NOTE.to_string();
    file.widths.entry(width_key(width_hours)).or_default().insert(
        bucket.id.clone(),
        BucketTopics {
            topics,
            notable,
            docs: bucket.images,
            sampled,
            fingerprint: fingerprint(&bucket.hashes),
            generated_at: now_iso.to_string(),
        },
    );
}

/// Every distinct topic and notable name across a width, most-used first. The corpus-level answer
/// to "what have I been working on", and what a `notable` name is for: it is a query you can then
/// run against the text index, the notes, or anything else.
pub fn vocabulary(file: &TopicsFile, width_hours: u32) -> (Vec<(String, usize)>, Vec<(String, usize)>) {
    let Some(stored) = file.widths.get(&width_key(width_hours)) else {
        return (Vec::new(), Vec::new());
    };
    let mut topics: BTreeMap<String, usize> = BTreeMap::new();
    let mut notable: BTreeMap<String, usize> = BTreeMap::new();
    for entry in stored.values() {
        for topic in &entry.topics {
            *topics.entry(topic.to_lowercase()).or_insert(0) += 1;
        }
        for name in &entry.notable {
            *notable.entry(name.clone()).or_insert(0) += 1;
        }
    }
    let rank = |map: BTreeMap<String, usize>| {
        let mut items: Vec<(String, usize)> = map.into_iter().collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        items
    };
    (rank(topics), rank(notable))
}

/// Convenience for callers that only want the timeline with topics already merged in.
pub fn timeline_with_topics(
    index: &text_index::TextIndex,
    options: &text_index::QueryOptions,
    width_hours: u32,
    file: &TopicsFile,
) -> Vec<Bucket> {
    // Hashes are needed for the fingerprint comparison, so they are always requested here and
    // dropped again below — the panel does not want 8,000 hashes crossing the IPC boundary.
    let mut buckets = text_index::timeline(index, options, width_hours, true);
    apply(&mut buckets, file, width_hours);
    buckets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(id: &str, hashes: &[&str]) -> Bucket {
        Bucket {
            id: id.to_string(),
            start: 0,
            end: 0,
            images: hashes.len(),
            representatives: hashes.len(),
            exact_dupes: 0,
            near_dupes: 0,
            chars: 0,
            terms: vec!["tauri".into(), "webview2".into()],
            hashes: hashes.iter().map(|h| h.to_string()).collect(),
            topics: Vec::new(),
            notable: Vec::new(),
            topics_stale: false,
        }
    }

    #[test]
    fn fingerprint_ignores_order_but_not_membership() {
        let a = fingerprint(&["one".into(), "two".into(), "three".into()]);
        let b = fingerprint(&["three".into(), "one".into(), "two".into()]);
        assert_eq!(a, b, "membership is a set; emission order is not part of it");
        let c = fingerprint(&["one".into(), "two".into()]);
        assert_ne!(a, c);
    }

    // Guards against the classic concatenation collision: ["ab","c"] and ["a","bc"] must not hash
    // the same, or a bucket losing one image and gaining another could read as unchanged.
    #[test]
    fn fingerprint_separates_boundaries() {
        assert_ne!(
            fingerprint(&["ab".into(), "c".into()]),
            fingerprint(&["a".into(), "bc".into()])
        );
    }

    #[test]
    fn a_bucket_that_gained_an_image_reads_as_stale() {
        let mut file = TopicsFile::default();
        let original = bucket("2026-08-03..04", &["aaa", "bbb"]);
        record(&mut file, 48, &original, vec!["indexing".into()], vec!["BM25".into()], 2, "m", "now");

        let mut unchanged = vec![original.clone()];
        apply(&mut unchanged, &file, 48);
        assert_eq!(unchanged[0].topics, vec!["indexing".to_string()]);
        assert!(!unchanged[0].topics_stale);

        let mut grown = vec![bucket("2026-08-03..04", &["aaa", "bbb", "ccc"])];
        apply(&mut grown, &file, 48);
        assert!(grown[0].topics_stale, "membership moved, so the answer is about a different set");
        assert_eq!(grown[0].topics, vec!["indexing".to_string()], "but it is still shown");
    }

    #[test]
    fn widths_do_not_overwrite_each_other() {
        let mut file = TopicsFile::default();
        let two_day = bucket("2026-08-03..04", &["aaa"]);
        let weekly = bucket("2026-08-03..09", &["aaa"]);
        record(&mut file, 48, &two_day, vec!["two-day".into()], vec![], 1, "m", "now");
        record(&mut file, 168, &weekly, vec!["weekly".into()], vec![], 1, "m", "now");
        assert_eq!(file.widths.len(), 2);

        let mut buckets = vec![two_day];
        apply(&mut buckets, &file, 48);
        assert_eq!(buckets[0].topics, vec!["two-day".to_string()]);
    }

    #[test]
    fn pending_skips_what_has_not_moved_unless_forced() {
        let mut file = TopicsFile::default();
        let done = bucket("2026-08-03..04", &["aaa"]);
        let fresh = bucket("2026-08-05..06", &["bbb"]);
        record(&mut file, 48, &done, vec!["x".into()], vec![], 1, "m", "now");

        let buckets = vec![done.clone(), fresh.clone()];
        let todo = pending(&buckets, &file, 48, false);
        assert_eq!(todo.len(), 1);
        assert_eq!(todo[0].id, "2026-08-05..06");
        assert_eq!(pending(&buckets, &file, 48, true).len(), 2, "force re-runs everything");
    }

    #[test]
    fn a_prompt_version_bump_discards_stored_answers() {
        let dir = std::env::temp_dir().join("icat-topics-test-version");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut file = TopicsFile::default();
        record(&mut file, 48, &bucket("2026-08-03..04", &["aaa"]), vec!["x".into()], vec![], 1, "m", "now");
        file.prompt_version = TOPIC_PROMPT_VERSION + 1;
        save(&dir, &file).unwrap();

        assert!(load(&dir).widths.is_empty(), "answers under another rubric must not be mixed in");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sampling_spreads_across_the_span_instead_of_taking_the_head() {
        let items: Vec<usize> = (0..100).collect();
        let picked = sample_evenly(&items, 5);
        assert_eq!(picked, vec![0, 20, 40, 60, 80]);
        assert_eq!(sample_evenly(&items, 200).len(), 100, "asking for more than exists returns all");
        assert!(sample_evenly(&items, 0).is_empty());
    }

    #[test]
    fn excerpt_drops_ocr_noise_rows_and_respects_the_limit() {
        let text = "Visual Studio Code\no\nx\nBM25 index over the corpus\nÜ\ntimeline buckets";
        let out = excerpt(text, 200);
        assert!(out.starts_with("Visual Studio Code"));
        assert!(out.contains("BM25 index over the corpus"));
        assert!(!out.contains(" o "), "one-character OCR noise rows are dropped: {out}");
        assert!(excerpt(text, 10).chars().count() <= 10);
    }

    #[test]
    fn a_fenced_reply_still_parses() {
        let reply = "Here is the JSON:\n```json\n{\"topics\": [\"search indexing\", \"tauri\"], \
                     \"notable\": [\"BM25\", \"WebView2\"]}\n```\nHope that helps!";
        let (topics, notable) = parse_reply(reply).unwrap();
        assert_eq!(topics, vec!["search indexing".to_string(), "tauri".to_string()]);
        assert_eq!(notable, vec!["BM25".to_string(), "WebView2".to_string()]);
    }

    #[test]
    fn a_length_stop_is_recognised_as_needing_more_room_not_as_a_failure() {
        // The exact message `vision::chat_text` builds when a reasoning model spends the whole
        // budget thinking. If that wording changes, the retry silently stops happening — so it is
        // pinned here rather than only at the call site.
        let real = "The model returned an empty reply (finish_reason: length). If it is a                     reasoning model, raise the token budget or pick one that answers directly.";
        assert!(is_out_of_room(real));
        assert!(!is_out_of_room("Request to http://localhost:1234 failed: connection refused"));
        assert!(!is_out_of_room("The model returned an empty reply (finish_reason: stop)."));
    }

    #[test]
    fn an_empty_or_prose_reply_is_an_error_not_an_empty_answer() {
        assert!(parse_reply("I could not determine any topics.").is_err());
        assert!(parse_reply("{\"topics\": [], \"notable\": []}").is_err());
    }

    #[test]
    fn the_prompt_asks_for_subjects_and_not_a_summary() {
        let sources = vec![TopicSource {
            at: "2026-08-03 14:22".into(),
            terms: vec!["bm25".into()],
            excerpt: "BM25F over the four note corpora".into(),
        }];
        let prompt = build_prompt("2026-08-03..04", &["bm25".to_string()], &sources);
        assert!(prompt.contains("NOT a summary"));
        assert!(prompt.contains("2026-08-03..04"));
        assert!(prompt.contains("BM25F over the four note corpora"));
    }

    #[test]
    fn vocabulary_ranks_by_how_many_buckets_used_a_name() {
        let mut file = TopicsFile::default();
        record(
            &mut file, 48, &bucket("a", &["1"]),
            vec!["tauri".into(), "indexing".into()], vec!["WebView2".into()], 1, "m", "now",
        );
        record(
            &mut file, 48, &bucket("b", &["2"]),
            vec!["Tauri".into()], vec!["WebView2".into(), "BM25".into()], 1, "m", "now",
        );
        let (topics, notable) = vocabulary(&file, 48);
        assert_eq!(topics[0], ("tauri".to_string(), 2), "case-folded so one subject counts once");
        assert_eq!(notable[0], ("WebView2".to_string(), 2));
    }
}
