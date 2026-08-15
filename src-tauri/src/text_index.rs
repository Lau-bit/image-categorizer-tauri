//! Full-text search over the OCR text the extraction pass already writes to
//! `.image-categorizer-ocr-text/<hash>.txt`.
//!
//! Scope is deliberately narrow: only the **High Text** category is indexed. That pool is 88% of
//! all extracted characters in 22% of the files (measured 2026-08-14: 14.3M of 16.25M chars over
//! 8,786 of 39,307 files, median 1,035 chars, 4 empty). Low Text is UI chrome and captions — 30
//! chars a piece — and exists to be *looked at*, not read; indexing it would quadruple the corpus
//! with noise. `LibraryConfig::text_index_categories` can widen this, but the default is the
//! intended target.
//!
//! Three things this module owns that are easy to get wrong later:
//!
//! 1. **Time is naive-local, on purpose.** Screenshot filenames (`screenshot_20260720_081329_…`)
//!    are already local civil time, and every one of the 14,051 High Text images parses. So a
//!    timestamp here is "local civil seconds since 1970" — the civil fields read straight out of
//!    the name with no timezone applied. Bucketing by calendar day is then exact and DST-free.
//!    `modified_ms` is only a fallback for a name that does not parse, flagged as `TS_SOURCE_MTIME`.
//!
//! 2. **Buckets are computed, never stored.** A "chunk" here is a time bucket at whatever width
//!    the caller asks for (48h by default — a working day that runs past midnight is still one
//!    sitting). Nothing persists a group, so changing the zoom cannot leave stale membership
//!    behind. The chunker's video-title groups are a different axis entirely and cover only 803 of
//!    14,051 High Text images (5.7%), so they are no use here.
//!
//! 3. **Duplicates are demoted, never deleted.** Consecutive screenshots of one screen repeat
//!    almost verbatim, and a naive result list is nothing but repeats. Exact repeats collapse under
//!    the earliest copy; near-repeats stay reachable and rank lower, because a near-repeat is
//!    often "the same screen, scrolled to include the line that was missing" — the delta is the
//!    payload. `novel_lines` recovers it on demand rather than storing it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub const TEXT_INDEX_FILE_NAME: &str = ".image-categorizer-text-index.json";
pub const TEXT_INDEX_SCHEMA_VERSION: u32 = 1;

/// The category the index covers unless the library config names others.
pub const DEFAULT_TEXT_CATEGORY: &str = "High Text";

/// Default bucket width. Two calendar days, so a session that crosses midnight stays one item.
pub const DEFAULT_BUCKET_HOURS: u32 = 48;

/// Okapi BM25 constants. Textbook defaults; this corpus gave no reason to tune them.
const K1: f32 = 1.2;
const B: f32 = 0.75;

/// Words per shingle for near-duplicate detection, and how much overlap makes two captures "the
/// same screen".
///
/// **Both numbers are measured, not assumed, and 5-grams at 0.8 was the wrong answer.** OCR does not
/// mangle text evenly — it scatters corrupted tokens through a page (`View`→`Wiew`,
/// `local-host`→`Iøcal-høst`), and a single bad token destroys every shingle containing it. At n=5
/// that is five; at n=3, three. Two captures of one screen one second apart measured **0.650 at n=5
/// but 0.703 at n=3**, so the original threshold missed the textbook case it existed for.
///
/// Sampled over the live library (6,752 consecutive High Text pairs under five minutes apart) the
/// n=3 distribution decays smoothly — 3,533 pairs near 0, 334 at 0.8 or above — with **no natural
/// cliff to snap to**. 0.6 is therefore a judgement, made toward recall for one reason: a near
/// duplicate is only *demoted*, never hidden, so a false positive costs a rank while a false
/// negative costs a repeat in the result list. It keeps the genuine re-capture above (0.703) and
/// still separates two busy desktop captures that merely share their chrome (measured 0.440).
const SHINGLE_N: usize = 3;
const NEAR_DUPE_JACCARD: f32 = 0.6;

/// Near-duplicate comparison is windowed over time rather than all-pairs (38M pairs at this corpus
/// size). Re-captures of one screen are minutes apart, so a window of this many
/// timestamp-neighbours inside `NEAR_DUPE_MAX_GAP_SECS` finds them. A near-repeat separated by more
/// than that is missed by design — `docs_compared` in the build report says how much was looked at.
const NEAR_DUPE_WINDOW: usize = 48;
const NEAR_DUPE_MAX_GAP_SECS: i64 = 48 * 3600;

/// How much a near-duplicate's score is multiplied by. Enough to sink it below any first-hand hit
/// while leaving it findable and reachable — a model asking for depth still gets it.
const NEAR_DUPE_SCORE_FACTOR: f32 = 0.35;

const MIN_TOKEN_LEN: usize = 2;
const MAX_TOKEN_LEN: usize = 40;

/// Keywords kept per document, statistical (tf-idf off the index's own term table). Free — no model
/// involved. The LLM topic layer is a later phase and writes its own sidecar.
const DOC_TOP_TERMS: usize = 8;
const BUCKET_TOP_TERMS: usize = 12;

pub const TS_SOURCE_FILENAME: u8 = 0;
pub const TS_SOURCE_MTIME: u8 = 1;

pub const RANK_REPRESENTATIVE: u8 = 0;
pub const RANK_EXACT_DUPE: u8 = 1;
pub const RANK_NEAR_DUPE: u8 = 2;

// Stopwords in both languages that actually appear on this machine's screens. Kept short on
// purpose: BM25's IDF already discounts common words, so a long list mostly costs recall on
// phrases. These are the ones frequent enough to bloat the postings for no ranking benefit.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was",
    "one", "our", "out", "has", "his", "how", "its", "new", "now", "old", "see", "two", "way",
    "who", "did", "get", "him", "let", "put", "say", "she", "too", "use", "with", "that", "this",
    "from", "they", "have", "were", "been", "will", "your", "what", "when", "which", "there",
    "their", "would", "about", "into", "than", "then", "them", "these", "some", "more", "only",
    "just", "also", "over", "such", "here",
    "ja", "on", "ei", "se", "että", "oli", "kun", "niin", "mutta", "tai", "jos", "kuin", "ovat",
    "olla", "joka", "sen", "the", "myös", "vain", "voi",
];

/// One image as the index sees it. The caller assembles these from the library view so this module
/// never has to know how a library is scanned.
#[derive(Debug, Clone)]
pub struct SourceDoc {
    pub hash: String,
    pub relative_path: String,
    pub name: String,
    pub folder: String,
    pub category: String,
    pub modified_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDoc {
    pub hash: String,
    pub path: String,
    pub name: String,
    pub folder: String,
    pub category: String,
    /// Local civil seconds since 1970 — see the module note on why this is not UTC.
    pub ts: i64,
    pub ts_source: u8,
    pub chars: u32,
    pub tokens: u32,
    /// Index of this document's group representative in `TextIndex::docs`. A document with no
    /// duplicates is its own representative.
    pub group: u32,
    pub rank: u8,
    pub terms: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    pub docs: usize,
    pub skipped_empty: usize,
    pub skipped_missing: usize,
    pub exact_dupes: usize,
    pub near_dupes: usize,
    pub groups: usize,
    pub terms: usize,
    pub total_chars: u64,
    pub docs_compared: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextIndex {
    pub version: u32,
    pub built_at: String,
    pub built_at_ms: u64,
    pub categories: Vec<String>,
    pub avg_len: f32,
    pub total_chars: u64,
    pub docs: Vec<IndexDoc>,
    pub terms: Vec<String>,
    /// Per term, a flat `[doc_index, term_frequency, …]` run. Flat rather than a struct because at
    /// ~900k postings the field names would be most of the file.
    pub postings: Vec<Vec<u32>>,
    pub report: BuildReport,

    #[serde(skip)]
    term_lookup: HashMap<String, usize>,
    #[serde(skip)]
    hash_lookup: HashMap<String, usize>,
}

impl TextIndex {
    fn rebuild_lookups(&mut self) {
        self.term_lookup = self
            .terms
            .iter()
            .enumerate()
            .map(|(index, term)| (term.clone(), index))
            .collect();
        self.hash_lookup = self
            .docs
            .iter()
            .enumerate()
            .map(|(index, doc)| (doc.hash.clone(), index))
            .collect();
    }

    pub fn doc_by_hash(&self, hash: &str) -> Option<(usize, &IndexDoc)> {
        self.hash_lookup.get(hash).map(|index| (*index, &self.docs[*index]))
    }

    /// Every document sharing `doc`'s duplicate group, representative first.
    pub fn group_members(&self, doc_index: usize) -> Vec<usize> {
        let group = self.docs[doc_index].group;
        let mut members: Vec<usize> = self
            .docs
            .iter()
            .enumerate()
            .filter(|(_, doc)| doc.group == group)
            .map(|(index, _)| index)
            .collect();
        members.sort_by_key(|index| (self.docs[*index].rank, self.docs[*index].ts));
        members
    }

    pub fn span(&self) -> Option<(i64, i64)> {
        let first = self.docs.iter().map(|doc| doc.ts).min()?;
        let last = self.docs.iter().map(|doc| doc.ts).max()?;
        Some((first, last))
    }
}

pub fn index_path(root: &Path) -> PathBuf {
    root.join(TEXT_INDEX_FILE_NAME)
}

pub fn load(root: &Path) -> Option<TextIndex> {
    let raw = fs::read_to_string(index_path(root)).ok()?;
    let mut index: TextIndex = serde_json::from_str(&raw).ok()?;
    if index.version != TEXT_INDEX_SCHEMA_VERSION {
        return None;
    }
    index.rebuild_lookups();
    Some(index)
}

pub fn save(root: &Path, index: &TextIndex) -> Result<(), String> {
    // Compact, not pretty: a pretty-printed postings table is several times the size for a file
    // nothing reads by eye. The docs list is the part a human might inspect, and `icat show`
    // prints that anyway.
    let data = serde_json::to_string(index)
        .map_err(|error| format!("Failed to serialize text index: {error}"))?;
    fs::write(index_path(root), data)
        .map_err(|error| format!("Failed to save text index: {error}"))
}

pub fn text_file_path(text_dir: &Path, hash: &str) -> PathBuf {
    text_dir.join(format!("{hash}.txt"))
}

// ---------------------------------------------------------------------------------------------
// Tokenizing
// ---------------------------------------------------------------------------------------------

pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                current.push(lower);
            }
        } else if !current.is_empty() {
            push_token(&mut tokens, &mut current);
        }
    }
    if !current.is_empty() {
        push_token(&mut tokens, &mut current);
    }
    tokens
}

fn push_token(tokens: &mut Vec<String>, current: &mut String) {
    // Counted in chars, not bytes: a two-byte `ä` is one character, and byte length would let
    // single-letter OCR noise through on any non-ASCII screen.
    let length = current.chars().count();
    if (MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&length) && !is_stopword(current) {
        tokens.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn is_stopword(token: &str) -> bool {
    STOPWORDS.contains(&token)
}

/// Whitespace-collapsed lowercase form. The exact-duplicate key, so that two captures of one screen
/// differing only in trailing blank lines still collapse.
fn normalize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            last_space = false;
        }
    }
    out.trim_end().to_string()
}

/// FNV-1a. Hand-rolled rather than `DefaultHasher` because these hashes decide duplicate groups
/// that are written to disk, and `DefaultHasher`'s output is explicitly not stable across Rust
/// versions — a toolchain bump would silently repartition every group on the next rebuild.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn shingles(tokens: &[String]) -> Vec<u64> {
    if tokens.len() < SHINGLE_N {
        // Too short to shingle: fall back to the whole token run so short documents can still match
        // each other exactly rather than silently never being near-duplicates.
        if tokens.is_empty() {
            return Vec::new();
        }
        return vec![fnv1a(tokens.join(" ").as_bytes())];
    }
    let mut out: Vec<u64> = tokens
        .windows(SHINGLE_N)
        .map(|window| fnv1a(window.join(" ").as_bytes()))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

fn jaccard(left: &[u64], right: &[u64]) -> f32 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut shared) = (0usize, 0usize, 0usize);
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Equal => {
                shared += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    let union = left.len() + right.len() - shared;
    if union == 0 {
        0.0
    } else {
        shared as f32 / union as f32
    }
}

// ---------------------------------------------------------------------------------------------
// Civil time. Local-naive throughout — see the module note.
// ---------------------------------------------------------------------------------------------

/// Howard Hinnant's days_from_civil. Proleptic Gregorian, era-based, exact for our range.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i64;
    let day = day as i64;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of `days_from_civil`.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

pub fn civil_to_secs(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    days_from_civil(year, month, day) * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64
}

pub fn format_date(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn format_datetime(ts: i64) -> String {
    let secs_of_day = ts.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}",
        format_date(ts),
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60
    )
}

/// Pulls the capture time out of a screenshot filename (`…_20260720_081329_…`). Every High Text
/// image in the live library carries one, which is why filename beats mtime: mtime moves when a
/// file is copied, the name does not.
pub fn timestamp_from_name(name: &str) -> Option<i64> {
    let bytes: Vec<char> = name.chars().collect();
    for start in 0..bytes.len() {
        // Looking for `YYYYMMDD_HHMMSS`.
        if start + 15 > bytes.len() {
            break;
        }
        let digits_ok = bytes[start..start + 8].iter().all(|c| c.is_ascii_digit())
            && bytes[start + 8] == '_'
            && bytes[start + 9..start + 15].iter().all(|c| c.is_ascii_digit());
        if !digits_ok {
            continue;
        }
        // A leading digit before the run means we matched the tail of a longer number.
        if start > 0 && bytes[start - 1].is_ascii_digit() {
            continue;
        }
        let date: String = bytes[start..start + 8].iter().collect();
        let time: String = bytes[start + 9..start + 15].iter().collect();
        let year: i64 = date[0..4].parse().ok()?;
        let month: u32 = date[4..6].parse().ok()?;
        let day: u32 = date[6..8].parse().ok()?;
        let hour: u32 = time[0..2].parse().ok()?;
        let minute: u32 = time[2..4].parse().ok()?;
        let second: u32 = time[4..6].parse().ok()?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
            continue;
        }
        return Some(civil_to_secs(year, month, day, hour, minute, second));
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Buckets — computed at query time, never stored.
// ---------------------------------------------------------------------------------------------

/// Where bucket boundaries are measured from: **1970-01-05, a Monday**, in local civil seconds.
///
/// Absolute, never derived from the corpus. An earlier version anchored on the midnight of the
/// earliest document, which meant importing one older screenshot shifted every boundary in the
/// library and silently re-partitioned the whole timeline — and once topics are stored per bucket,
/// it would invalidate all of them at once. A fixed epoch makes a bucket label mean the same span
/// forever.
///
/// The Monday matters only for week-width buckets, where it is the difference between
/// Monday–Sunday and Thursday–Wednesday. Day and two-day widths are unaffected by the offset being
/// a whole number of days.
const BUCKET_EPOCH: i64 = 4 * 86_400;

#[derive(Debug, Clone, Copy)]
pub struct BucketSpec {
    pub width_secs: i64,
    pub anchor: i64,
}

impl BucketSpec {
    pub fn new(width_hours: u32) -> Self {
        Self {
            width_secs: (width_hours.max(1) as i64) * 3600,
            anchor: BUCKET_EPOCH,
        }
    }

    pub fn bucket_of(&self, ts: i64) -> i64 {
        (ts - self.anchor).div_euclid(self.width_secs)
    }

    pub fn start_of(&self, bucket: i64) -> i64 {
        self.anchor + bucket * self.width_secs
    }

    pub fn label(&self, bucket: i64) -> String {
        let start = self.start_of(bucket);
        let end = start + self.width_secs - 1;
        if self.width_secs < 86_400 {
            let start_secs = start.rem_euclid(86_400);
            let end_secs = end.rem_euclid(86_400) + 1;
            return format!(
                "{} {:02}:{:02}–{:02}:{:02}",
                format_date(start),
                start_secs / 3600,
                (start_secs % 3600) / 60,
                end_secs / 3600,
                (end_secs % 3600) / 60
            );
        }
        if self.width_secs == 86_400 {
            return format_date(start);
        }
        let (start_year, start_month, start_day) = civil_from_days(start.div_euclid(86_400));
        let (end_year, end_month, end_day) = civil_from_days(end.div_euclid(86_400));
        if start_year == end_year && start_month == end_month {
            format!("{start_year:04}-{start_month:02}-{start_day:02}..{end_day:02}")
        } else if start_year == end_year {
            format!("{start_year:04}-{start_month:02}-{start_day:02}..{end_month:02}-{end_day:02}")
        } else {
            format!(
                "{start_year:04}-{start_month:02}-{start_day:02}..{end_year:04}-{end_month:02}-{end_day:02}"
            )
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------------------------

struct Parsed {
    source: SourceDoc,
    ts: i64,
    ts_source: u8,
    chars: u32,
    tokens: Vec<String>,
    shingles: Vec<u64>,
    norm_hash: u64,
}

pub fn build(sources: &[SourceDoc], text_dir: &Path, categories: &[String], now_ms: u64, now_iso: &str) -> TextIndex {
    let started = std::time::Instant::now();

    let allowed: HashSet<&str> = categories.iter().map(String::as_str).collect();
    let eligible: Vec<&SourceDoc> = sources
        .iter()
        .filter(|source| allowed.contains(source.category.as_str()))
        .collect();

    // Reading and tokenizing ~9k files is the whole cost of a build; rayon takes it from seconds to
    // well under one on this machine.
    let parsed: Vec<Option<Parsed>> = eligible
        .par_iter()
        .map(|source| {
            let path = text_file_path(text_dir, &source.hash);
            let text = fs::read_to_string(&path).ok()?;
            let normalized = normalize_text(&text);
            if normalized.is_empty() {
                return None;
            }
            let tokens = tokenize(&text);
            if tokens.is_empty() {
                return None;
            }
            let shingles = shingles(&tokens);
            let (ts, ts_source) = match timestamp_from_name(&source.name) {
                Some(ts) => (ts, TS_SOURCE_FILENAME),
                None => ((source.modified_ms / 1000) as i64, TS_SOURCE_MTIME),
            };
            Some(Parsed {
                source: (*source).clone(),
                ts,
                ts_source,
                chars: text.chars().count() as u32,
                tokens,
                shingles,
                norm_hash: fnv1a(normalized.as_bytes()),
            })
        })
        .collect();

    let missing = parsed.iter().filter(|item| item.is_none()).count();
    let mut parsed: Vec<Parsed> = parsed.into_iter().flatten().collect();
    // Chronological order is what the near-duplicate window slides along, and it makes the stored
    // docs list readable top to bottom.
    parsed.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.source.hash.cmp(&b.source.hash)));

    let doc_count = parsed.len();
    let mut parent: Vec<usize> = (0..doc_count).collect();

    // Exact duplicates first, globally — a HashMap over the normalized-text hash, so distance in
    // time is irrelevant for these.
    let mut by_norm: HashMap<u64, usize> = HashMap::new();
    for (index, item) in parsed.iter().enumerate() {
        match by_norm.get(&item.norm_hash) {
            Some(first) => union(&mut parent, *first, index),
            None => {
                by_norm.insert(item.norm_hash, index);
            }
        }
    }

    // Near duplicates, windowed over the chronological order.
    let mut docs_compared = 0usize;
    for index in 0..doc_count {
        let start = index.saturating_sub(NEAR_DUPE_WINDOW);
        for other in start..index {
            if parsed[index].ts - parsed[other].ts > NEAR_DUPE_MAX_GAP_SECS {
                continue;
            }
            if find(&mut parent, index) == find(&mut parent, other) {
                continue;
            }
            docs_compared += 1;
            if jaccard(&parsed[index].shingles, &parsed[other].shingles) >= NEAR_DUPE_JACCARD {
                union(&mut parent, index, other);
            }
        }
    }

    // Representative per group: earliest capture. `parsed` is already sorted by ts, so the lowest
    // member index is the earliest.
    let mut representative: HashMap<usize, usize> = HashMap::new();
    for index in 0..doc_count {
        let root = find(&mut parent, index);
        representative.entry(root).or_insert(index);
    }

    // Postings and document frequencies.
    let mut term_lookup: HashMap<String, usize> = HashMap::new();
    let mut terms: Vec<String> = Vec::new();
    let mut postings: Vec<Vec<u32>> = Vec::new();
    let mut doc_term_counts: Vec<Vec<(usize, u32)>> = Vec::with_capacity(doc_count);

    for (doc_index, item) in parsed.iter().enumerate() {
        let mut counts: BTreeMap<usize, u32> = BTreeMap::new();
        for token in &item.tokens {
            let term_index = match term_lookup.get(token) {
                Some(index) => *index,
                None => {
                    let index = terms.len();
                    terms.push(token.clone());
                    postings.push(Vec::new());
                    term_lookup.insert(token.clone(), index);
                    index
                }
            };
            *counts.entry(term_index).or_insert(0) += 1;
        }
        let flat: Vec<(usize, u32)> = counts.into_iter().collect();
        for (term_index, tf) in &flat {
            postings[*term_index].push(doc_index as u32);
            postings[*term_index].push(*tf);
        }
        doc_term_counts.push(flat);
    }

    let total_tokens: u64 = parsed.iter().map(|item| item.tokens.len() as u64).sum();
    let avg_len = if doc_count == 0 { 0.0 } else { total_tokens as f32 / doc_count as f32 };

    // Statistical keywords: plain tf-idf over the table we just built. This is what makes a
    // per-image keyword list free — no model, no second pass, no stored vocabulary of its own.
    let doc_freq: Vec<u32> = postings.iter().map(|list| (list.len() / 2) as u32).collect();

    let mut docs: Vec<IndexDoc> = Vec::with_capacity(doc_count);
    let mut exact_dupes = 0usize;
    let mut near_dupes = 0usize;

    for (doc_index, item) in parsed.iter().enumerate() {
        let root = find(&mut parent, doc_index);
        let rep = representative[&root];
        let rank = if rep == doc_index {
            RANK_REPRESENTATIVE
        } else if parsed[rep].norm_hash == item.norm_hash {
            exact_dupes += 1;
            RANK_EXACT_DUPE
        } else {
            near_dupes += 1;
            RANK_NEAR_DUPE
        };

        let mut scored: Vec<(f32, usize)> = doc_term_counts[doc_index]
            .iter()
            .map(|(term_index, tf)| {
                let idf = idf_for(doc_freq[*term_index], doc_count);
                (*tf as f32 * idf, *term_index)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let top_terms: Vec<String> = scored
            .iter()
            .take(DOC_TOP_TERMS)
            .map(|(_, term_index)| terms[*term_index].clone())
            .collect();

        docs.push(IndexDoc {
            hash: item.source.hash.clone(),
            path: item.source.relative_path.clone(),
            name: item.source.name.clone(),
            folder: item.source.folder.clone(),
            category: item.source.category.clone(),
            ts: item.ts,
            ts_source: item.ts_source,
            chars: item.chars,
            tokens: item.tokens.len() as u32,
            group: rep as u32,
            rank,
            terms: top_terms,
        });
    }

    let groups = representative.len();
    let total_chars: u64 = docs.iter().map(|doc| doc.chars as u64).sum();

    let mut index = TextIndex {
        version: TEXT_INDEX_SCHEMA_VERSION,
        built_at: now_iso.to_string(),
        built_at_ms: now_ms,
        categories: categories.to_vec(),
        avg_len,
        total_chars,
        docs,
        terms,
        postings,
        report: BuildReport {
            docs: doc_count,
            skipped_empty: 0,
            skipped_missing: missing,
            exact_dupes,
            near_dupes,
            groups,
            terms: 0,
            total_chars,
            docs_compared,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        term_lookup: HashMap::new(),
        hash_lookup: HashMap::new(),
    };
    index.report.terms = index.terms.len();
    index.rebuild_lookups();
    index
}

fn find(parent: &mut Vec<usize>, mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

fn union(parent: &mut Vec<usize>, a: usize, b: usize) {
    let (root_a, root_b) = (find(parent, a), find(parent, b));
    if root_a == root_b {
        return;
    }
    // Lower index wins, so the group root stays the earliest document and representatives are
    // stable regardless of the order unions happen in.
    if root_a < root_b {
        parent[root_b] = root_a;
    } else {
        parent[root_a] = root_b;
    }
}

fn idf_for(doc_freq: u32, doc_count: usize) -> f32 {
    let n = doc_count as f32;
    let df = doc_freq as f32;
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

// ---------------------------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct QueryOptions {
    pub query: String,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub folders: Vec<String>,
    pub categories: Vec<String>,
    pub min_chars: u32,
    /// Include exact duplicates, which are collapsed under their representative by default.
    pub include_dupes: bool,
    /// Require every query term (AND) rather than scoring a union.
    pub require_all: bool,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub doc: usize,
    pub hash: String,
    pub path: String,
    pub name: String,
    pub ts: i64,
    pub score: f32,
    pub chars: u32,
    pub rank: u8,
    pub terms: Vec<String>,
    /// Members of this hit's duplicate group that are not the hit itself.
    pub exact_dupes: usize,
    pub near_dupes: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// How many documents scored above zero and passed the filters, before `limit` was applied.
    pub matched: usize,
    /// Query terms that are not in the index at all — the difference between "no results" and
    /// "you searched for a word that was never on screen".
    pub unknown_terms: Vec<String>,
    /// Set when a quoted phrase was verified against only the highest-scoring candidates. A phrase
    /// cannot be answered from the postings, so verification costs one file read each; this says
    /// plainly how many were looked at rather than implying the whole corpus was.
    pub phrase_checked: usize,
    pub phrase_capped: bool,
}

/// How many candidates a quoted phrase is verified against, in score order.
const PHRASE_VERIFY_LIMIT: usize = 400;

pub fn search(index: &TextIndex, options: &QueryOptions, text_dir: Option<&Path>) -> SearchOutcome {
    let mut outcome = SearchOutcome::default();
    let (terms, phrases) = split_query(&options.query);
    if terms.is_empty() {
        return outcome;
    }

    let doc_count = index.docs.len();
    let mut scores: HashMap<usize, f32> = HashMap::new();
    let mut matched_terms: HashMap<usize, usize> = HashMap::new();
    let mut known_terms = 0usize;

    for term in &terms {
        let Some(term_index) = index.term_lookup.get(term) else {
            outcome.unknown_terms.push(term.clone());
            continue;
        };
        known_terms += 1;
        let list = &index.postings[*term_index];
        let doc_freq = (list.len() / 2) as u32;
        let idf = idf_for(doc_freq, doc_count);
        for pair in list.chunks_exact(2) {
            let doc_index = pair[0] as usize;
            let tf = pair[1] as f32;
            let len = index.docs[doc_index].tokens as f32;
            let denominator = tf + K1 * (1.0 - B + B * len / index.avg_len.max(1.0));
            let contribution = idf * (tf * (K1 + 1.0)) / denominator.max(f32::EPSILON);
            *scores.entry(doc_index).or_insert(0.0) += contribution;
            *matched_terms.entry(doc_index).or_insert(0) += 1;
        }
    }

    let mut hits: Vec<Hit> = Vec::new();
    for (doc_index, score) in scores {
        let doc = &index.docs[doc_index];
        if options.require_all && matched_terms.get(&doc_index).copied().unwrap_or(0) < known_terms {
            continue;
        }
        if !passes_search_filters(doc, options) {
            continue;
        }
        let mut score = score;
        if doc.rank == RANK_NEAR_DUPE {
            score *= NEAR_DUPE_SCORE_FACTOR;
        }
        hits.push(Hit {
            doc: doc_index,
            hash: doc.hash.clone(),
            path: doc.path.clone(),
            name: doc.name.clone(),
            ts: doc.ts,
            score,
            chars: doc.chars,
            rank: doc.rank,
            terms: doc.terms.clone(),
            exact_dupes: 0,
            near_dupes: 0,
        });
    }

    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // A quoted phrase cannot be answered from the postings — word positions are not stored — so it
    // is verified by reading the candidate's own text, in score order and capped. The terms inside
    // the quotes already narrowed the candidate set above, so the cap almost never binds; when it
    // does, `phrase_capped` says so rather than presenting a truncated set as complete.
    if !phrases.is_empty() {
        if let Some(text_dir) = text_dir {
            let mut kept: Vec<Hit> = Vec::new();
            for (position, hit) in hits.iter().enumerate() {
                if position >= PHRASE_VERIFY_LIMIT {
                    outcome.phrase_capped = true;
                    break;
                }
                outcome.phrase_checked += 1;
                let Ok(text) = fs::read_to_string(text_file_path(text_dir, &hit.hash)) else {
                    continue;
                };
                let normalized = normalize_text(&text);
                if phrases.iter().all(|phrase| normalized.contains(phrase.as_str())) {
                    kept.push(hit.clone());
                }
            }
            hits = kept;
        }
    }

    outcome.matched = hits.len();
    let limit = if options.limit == 0 { hits.len() } else { options.limit };
    hits.truncate(limit);
    annotate_groups(index, &mut hits);
    outcome.hits = hits;
    outcome
}

/// Fills in each hit's duplicate counts. Kept off the scoring loop because it walks the group, and
/// only the hits actually being shown need it.
fn annotate_groups(index: &TextIndex, hits: &mut [Hit]) {
    let mut by_group: HashMap<u32, (usize, usize)> = HashMap::new();
    for doc in &index.docs {
        let entry = by_group.entry(doc.group).or_insert((0, 0));
        match doc.rank {
            RANK_EXACT_DUPE => entry.0 += 1,
            RANK_NEAR_DUPE => entry.1 += 1,
            _ => {}
        }
    }
    for hit in hits.iter_mut() {
        let group = index.docs[hit.doc].group;
        if let Some((exact, near)) = by_group.get(&group) {
            hit.exact_dupes = *exact;
            hit.near_dupes = *near;
        }
    }
}

/// Scope only: time, folder, category, length. Deliberately says nothing about duplicates, because
/// the two callers want opposite things — a result list must not spend slots on identical copies,
/// while a timeline answering "how much did I capture that day" must count every one of them.
fn passes_scope(doc: &IndexDoc, options: &QueryOptions) -> bool {
    if let Some(from) = options.from {
        if doc.ts < from {
            return false;
        }
    }
    if let Some(to) = options.to {
        if doc.ts > to {
            return false;
        }
    }
    if doc.chars < options.min_chars {
        return false;
    }
    if !options.folders.is_empty() && !options.folders.iter().any(|folder| folder == &doc.folder) {
        return false;
    }
    if !options.categories.is_empty() && !options.categories.iter().any(|c| c == &doc.category) {
        return false;
    }
    true
}

/// Scope plus the result-list duplicate rule: exact duplicates are hidden unless asked for, since
/// they carry nothing the representative does not. Near duplicates always survive — they are
/// demoted by score, never filtered, because their delta is often the reason to keep them.
fn passes_search_filters(doc: &IndexDoc, options: &QueryOptions) -> bool {
    passes_scope(doc, options) && !(doc.rank == RANK_EXACT_DUPE && !options.include_dupes)
}

/// Splits a raw query into bare terms and quoted phrases.
pub fn split_query(query: &str) -> (Vec<String>, Vec<String>) {
    let mut terms: Vec<String> = Vec::new();
    let mut phrases: Vec<String> = Vec::new();
    let mut rest = String::new();
    let mut in_quote = false;
    let mut current = String::new();
    for ch in query.chars() {
        if ch == '"' {
            if in_quote {
                phrases.push(normalize_text(&current));
                terms.extend(tokenize(&current));
                current.clear();
            }
            in_quote = !in_quote;
            continue;
        }
        if in_quote {
            current.push(ch);
        } else {
            rest.push(ch);
        }
    }
    if in_quote && !current.is_empty() {
        terms.extend(tokenize(&current));
    }
    terms.extend(tokenize(&rest));
    terms.sort();
    terms.dedup();
    (terms, phrases)
}

// ---------------------------------------------------------------------------------------------
// Snippets and deltas
// ---------------------------------------------------------------------------------------------

/// The window of `text` densest in `terms`, as a snippet of about `width` characters. Falls back to
/// the head of the document when nothing matches (a phrase-only or filter-only result).
pub fn snippet(text: &str, terms: &[String], width: usize) -> String {
    let lower = text.to_lowercase();
    let mut best_start = 0usize;
    let mut best_score = 0usize;

    if !terms.is_empty() {
        let positions: Vec<usize> = terms
            .iter()
            .flat_map(|term| {
                let mut found = Vec::new();
                let mut from = 0usize;
                while let Some(offset) = lower[from..].find(term.as_str()) {
                    let at = from + offset;
                    found.push(at);
                    from = at + term.len().max(1);
                    if found.len() > 64 {
                        break;
                    }
                }
                found
            })
            .collect();

        for candidate in &positions {
            let start = candidate.saturating_sub(width / 4);
            let end = (start + width).min(lower.len());
            let score = positions.iter().filter(|at| **at >= start && **at < end).count();
            if score > best_score {
                best_score = score;
                best_start = start;
            }
        }
    }

    let start = floor_char_boundary(text, best_start);
    let end = floor_char_boundary(text, (start + width).min(text.len()));
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    // OCR text is one line per recognized line; a snippet reads far better as a single run.
    out.push_str(text[start..end].split_whitespace().collect::<Vec<_>>().join(" ").as_str());
    if end < text.len() {
        out.push('…');
    }
    out
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Lines present in `member` but not in `representative`, normalized for comparison but returned
/// verbatim. This is what makes a near-duplicate worth keeping: the usual case is the same screen
/// scrolled a little, and these lines are the only part that is new.
pub fn novel_lines(representative: &str, member: &str) -> Vec<String> {
    let known: HashSet<String> = representative
        .lines()
        .map(normalize_text)
        .filter(|line| !line.is_empty())
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    member
        .lines()
        .filter_map(|line| {
            let normalized = normalize_text(line);
            if normalized.is_empty() || known.contains(&normalized) || !seen.insert(normalized) {
                None
            } else {
                Some(line.trim().to_string())
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub id: String,
    pub start: i64,
    pub end: i64,
    pub images: usize,
    pub representatives: usize,
    pub exact_dupes: usize,
    pub near_dupes: usize,
    pub chars: u64,
    /// Statistical keywords — plain tf-idf, always present, no model involved.
    pub terms: Vec<String>,
    pub hashes: Vec<String>,
    /// Filled in by the topic layer when it has run for this width (see `topics.rs`). Left empty
    /// rather than defaulted to `terms`, so "no topics yet" stays distinguishable from "the model
    /// looked at this and found these" — the panel shows whichever it actually has.
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub notable: Vec<String>,
    /// True when topics exist but were written against a different set of images than the bucket
    /// holds now.
    #[serde(default)]
    pub topics_stale: bool,
}

pub fn timeline(index: &TextIndex, options: &QueryOptions, width_hours: u32, with_hashes: bool) -> Vec<Bucket> {
    let spec = BucketSpec::new(width_hours);
    let doc_count = index.docs.len();

    // Document frequency straight off the postings, so bucket keywords use the same tf-idf the
    // per-document ones do.
    let doc_freq: Vec<u32> = index.postings.iter().map(|list| (list.len() / 2) as u32).collect();
    let mut term_weight: HashMap<i64, HashMap<usize, f32>> = HashMap::new();

    let mut grouped: BTreeMap<i64, Bucket> = BTreeMap::new();
    for doc in index.docs.iter() {
        // A timeline honours the same filters a search does, minus the query itself — that is what
        // makes "everything from these two days, nothing else" a single call.
        if !passes_scope(doc, options) {
            continue;
        }
        let bucket_id = spec.bucket_of(doc.ts);
        let entry = grouped.entry(bucket_id).or_insert_with(|| Bucket {
            id: spec.label(bucket_id),
            start: spec.start_of(bucket_id),
            end: spec.start_of(bucket_id) + spec.width_secs - 1,
            images: 0,
            representatives: 0,
            exact_dupes: 0,
            near_dupes: 0,
            chars: 0,
            terms: Vec::new(),
            hashes: Vec::new(),
            topics: Vec::new(),
            notable: Vec::new(),
            topics_stale: false,
        });
        entry.images += 1;
        entry.chars += doc.chars as u64;
        match doc.rank {
            RANK_EXACT_DUPE => entry.exact_dupes += 1,
            RANK_NEAR_DUPE => entry.near_dupes += 1,
            _ => entry.representatives += 1,
        }
        if with_hashes {
            entry.hashes.push(doc.hash.clone());
        }
        // Only representatives contribute keywords, or a screen captured twenty times would decide
        // what the whole bucket was "about".
        if doc.rank == RANK_REPRESENTATIVE {
            let weights = term_weight.entry(bucket_id).or_default();
            for term in &doc.terms {
                if let Some(term_index) = index.term_lookup.get(term) {
                    let idf = idf_for(doc_freq[*term_index], doc_count);
                    *weights.entry(*term_index).or_insert(0.0) += idf;
                }
            }
        }
    }

    for (bucket_id, bucket) in grouped.iter_mut() {
        if let Some(weights) = term_weight.get(bucket_id) {
            let mut scored: Vec<(f32, usize)> = weights.iter().map(|(index, weight)| (*weight, *index)).collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            bucket.terms = scored
                .iter()
                .take(BUCKET_TOP_TERMS)
                .map(|(_, term_index)| index.terms[*term_index].clone())
                .collect();
        }
    }

    grouped.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(hash: &str, name: &str) -> SourceDoc {
        SourceDoc {
            hash: hash.to_string(),
            relative_path: format!("folder/{name}"),
            name: name.to_string(),
            folder: "folder".to_string(),
            category: DEFAULT_TEXT_CATEGORY.to_string(),
            modified_ms: 0,
        }
    }

    fn build_in(dir: &Path, files: &[(&str, &str, &str)]) -> TextIndex {
        fs::create_dir_all(dir).unwrap();
        let sources: Vec<SourceDoc> = files
            .iter()
            .map(|(hash, name, text)| {
                fs::write(text_file_path(dir, hash), text).unwrap();
                source(hash, name)
            })
            .collect();
        build(&sources, dir, &[DEFAULT_TEXT_CATEGORY.to_string()], 0, "1970-01-01T00:00:00Z")
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("icat-text-index-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn filename_timestamp_is_read_as_local_civil_time() {
        let ts = timestamp_from_name("screenshot_20260720_081329_743_window.png").unwrap();
        assert_eq!(format_datetime(ts), "2026-07-20 08:13");
    }

    #[test]
    fn filename_timestamp_ignores_a_longer_digit_run() {
        // A hash or counter that happens to contain a date-shaped substring must not be read as one.
        assert!(timestamp_from_name("123420260720_081329x.png").is_none());
    }

    #[test]
    fn civil_round_trips() {
        for days in [-1, 0, 1, 20_000, 20_650] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
    }

    #[test]
    fn buckets_are_calendar_aligned_and_two_days_wide() {
        let first = civil_to_secs(2026, 8, 3, 9, 30, 0);
        let spec = BucketSpec::new(48);
        let same_bucket = civil_to_secs(2026, 8, 4, 2, 15, 0);
        let next_bucket = civil_to_secs(2026, 8, 5, 1, 0, 0);
        assert_eq!(spec.bucket_of(first), spec.bucket_of(same_bucket), "past midnight is the same sitting");
        assert_ne!(spec.bucket_of(first), spec.bucket_of(next_bucket));
        assert_eq!(spec.label(spec.bucket_of(first)), "2026-08-03..04");
    }

    #[test]
    fn bucket_label_spans_a_month_boundary() {
        let spec = BucketSpec::new(48);
        let bucket = spec.bucket_of(civil_to_secs(2026, 8, 31, 12, 0, 0));
        assert_eq!(spec.label(bucket), "2026-08-31..09-01");
    }

    #[test]
    fn exact_duplicates_collapse_and_are_hidden_by_default() {
        let dir = temp_dir("exact");
        let text = "vault search bm25f corpus retrieval loopback service";
        let index = build_in(
            &dir,
            &[
                ("aaa", "screenshot_20260803_090000_000.png", text),
                ("bbb", "screenshot_20260803_091000_000.png", text),
            ],
        );
        assert_eq!(index.report.exact_dupes, 1);
        assert_eq!(index.report.groups, 1);

        let options = QueryOptions { query: "bm25f".into(), ..Default::default() };
        let outcome = search(&index, &options, Some(&dir));
        assert_eq!(outcome.hits.len(), 1, "the duplicate must not take a second slot");
        assert_eq!(outcome.hits[0].exact_dupes, 1, "but it must still be counted");

        let with_dupes = QueryOptions { include_dupes: true, ..options };
        assert_eq!(search(&index, &with_dupes, Some(&dir)).hits.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_near_duplicate_is_demoted_but_still_returned() {
        let dir = temp_dir("near");
        let base: Vec<String> = (0..40).map(|i| format!("token{i}")).collect();
        let first = format!("{} marker", base.join(" "));
        let second = format!("{} marker extra scrolled line", base.join(" "));
        let index = build_in(
            &dir,
            &[
                ("aaa", "screenshot_20260803_090000_000.png", first.as_str()),
                ("bbb", "screenshot_20260803_090500_000.png", second.as_str()),
            ],
        );
        assert_eq!(index.report.near_dupes, 1, "shingle overlap must group these");

        let outcome = search(&index, &QueryOptions { query: "marker".into(), ..Default::default() }, Some(&dir));
        assert_eq!(outcome.hits.len(), 2, "near duplicates stay reachable");
        assert_eq!(outcome.hits[0].rank, RANK_REPRESENTATIVE);
        assert_eq!(outcome.hits[1].rank, RANK_NEAR_DUPE);
        assert!(outcome.hits[0].score > outcome.hits[1].score, "and rank below first-hand hits");
        let _ = fs::remove_dir_all(&dir);
    }

    // Pins the discrimination the shingle constants were tuned for, using the shape measured on the
    // live library: OCR corrupts scattered tokens, so a genuine re-capture is far from identical.
    // If someone raises the threshold back toward 0.8, or SHINGLE_N back to 5, this fails.
    #[test]
    fn ocr_noise_does_not_stop_a_recapture_from_grouping() {
        let dir = temp_dir("ocrnoise");
        let clean: Vec<String> = (0..60).map(|i| format!("word{i}")).collect();
        // One token in fifteen mangled, the way the OCR engine actually mangles them. Every
        // corrupted token takes three 3-grams with it, which puts this pair around 0.66 — close to
        // the real re-capture measured at 0.703, and deliberately not far above the 0.6 threshold,
        // so the test fails if the threshold creeps back up.
        let noisy: Vec<String> = clean
            .iter()
            .enumerate()
            .map(|(position, token)| if position % 15 == 0 { format!("{token}x") } else { token.clone() })
            .collect();
        let index = build_in(
            &dir,
            &[
                ("aaa", "screenshot_20260803_090000_000.png", clean.join(" ").as_str()),
                ("bbb", "screenshot_20260803_090001_000.png", noisy.join(" ").as_str()),
            ],
        );
        assert_eq!(index.report.near_dupes, 1, "a re-capture with OCR noise must still group");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_busy_screens_sharing_only_chrome_stay_apart() {
        let dir = temp_dir("distinct");
        let chrome: Vec<String> = (0..30).map(|i| format!("chrome{i}")).collect();
        let first: Vec<String> = (0..70).map(|i| format!("alpha{i}")).collect();
        let second: Vec<String> = (0..70).map(|i| format!("beta{i}")).collect();
        let index = build_in(
            &dir,
            &[
                ("aaa", "screenshot_20260803_090000_000.png", format!("{} {}", chrome.join(" "), first.join(" ")).as_str()),
                ("bbb", "screenshot_20260803_090100_000.png", format!("{} {}", chrome.join(" "), second.join(" ")).as_str()),
            ],
        );
        assert_eq!(index.report.near_dupes, 0, "shared window chrome is not shared content");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn novel_lines_recovers_only_what_the_scroll_added() {
        let representative = "Alpha line\nBeta line\nGamma line";
        let member = "Beta line\nGamma line\nDelta line\nEpsilon line";
        assert_eq!(novel_lines(representative, member), vec!["Delta line", "Epsilon line"]);
    }

    #[test]
    fn unknown_terms_are_reported_rather_than_scoring_nothing() {
        let dir = temp_dir("unknown");
        let index = build_in(&dir, &[("aaa", "screenshot_20260803_090000_000.png", "webview2 debugging notes")]);
        let outcome = search(&index, &QueryOptions { query: "webview2 kubernetes".into(), ..Default::default() }, Some(&dir));
        assert_eq!(outcome.unknown_terms, vec!["kubernetes".to_string()]);
        assert_eq!(outcome.hits.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn time_filters_bound_the_result_set() {
        let dir = temp_dir("time");
        let index = build_in(
            &dir,
            &[
                ("aaa", "screenshot_20260701_090000_000.png", "alpha topic one"),
                ("bbb", "screenshot_20260803_090000_000.png", "alpha topic two"),
            ],
        );
        let options = QueryOptions {
            query: "alpha".into(),
            from: Some(civil_to_secs(2026, 8, 1, 0, 0, 0)),
            ..Default::default()
        };
        let outcome = search(&index, &options, Some(&dir));
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].hash, "bbb");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn timeline_buckets_only_count_representatives_toward_keywords() {
        let dir = temp_dir("timeline");
        let text = "cockpit feed contract retrieval";
        let index = build_in(
            &dir,
            &[
                ("aaa", "screenshot_20260803_090000_000.png", text),
                ("bbb", "screenshot_20260803_093000_000.png", text),
                ("ccc", "screenshot_20260803_094000_000.png", "unrelated gardening notes"),
            ],
        );
        let buckets = timeline(&index, &QueryOptions::default(), 48, false);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].images, 3);
        assert_eq!(buckets[0].exact_dupes, 1);
        assert_eq!(buckets[0].representatives, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn require_all_narrows_a_union_query() {
        let dir = temp_dir("requireall");
        let index = build_in(
            &dir,
            &[
                ("aaa", "screenshot_20260803_090000_000.png", "tauri webview2 debug port"),
                ("bbb", "screenshot_20260803_091000_000.png", "tauri window bounds"),
            ],
        );
        let any = search(&index, &QueryOptions { query: "tauri webview2".into(), ..Default::default() }, Some(&dir));
        assert_eq!(any.hits.len(), 2);
        let all = search(
            &index,
            &QueryOptions { query: "tauri webview2".into(), require_all: true, ..Default::default() },
            Some(&dir),
        );
        assert_eq!(all.hits.len(), 1);
        assert_eq!(all.hits[0].hash, "aaa");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn snippet_centres_on_the_query_rather_than_the_head() {
        let filler = "filler ".repeat(120);
        let text = format!("{filler}the needle appears here{filler}");
        let out = snippet(&text, &["needle".to_string()], 120);
        assert!(out.contains("needle"), "snippet should hold the match: {out}");
    }
}
