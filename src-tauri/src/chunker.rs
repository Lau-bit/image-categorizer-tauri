//! Video-frame de-duplication by on-screen title.
//!
//! Many screenshots in the library are frames grabbed from the *same* YouTube video (driving /
//! walking / train footage), whose borderless-browser title bar shows the video title — e.g.
//! "Driving across the Pyrenees mountains from France to Andorra - YouTube". OCRing that top strip
//! (see `ocr::extract_title_strip`) lets us group every frame of one video together and keep only a
//! small random sample for the expensive per-image vision pass, instead of describing 500 nearly
//! identical frames.
//!
//! Everything here is pure and deterministic (no clock, no RNG state): the sample a group yields is
//! a stable pseudo-random function of its member hashes, so re-running without discarding the plan
//! reproduces the same selection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Site suffixes that confirm a title bar belongs to a playing video. Matched case-insensitively;
/// the title is everything before the marker.
const VIDEO_MARKERS: &[&str] = &[" - youtube", " – youtube", " — youtube", " - vimeo", " - youtube music"];

/// Case-insensitive byte search returning the index into `haystack`. `needle` must be ASCII (all
/// our markers are), which guarantees the returned index lands on a char boundary safe to slice at.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    'outer: for i in 0..=(h.len() - n.len()) {
        for j in 0..n.len() {
            if !h[i + j].eq_ignore_ascii_case(&n[j]) {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

/// Collapses internal runs of whitespace to single spaces and trims the ends.
fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extracts the video title from a raw title-strip OCR reading, or `None` when the strip carries no
/// known video-site marker (so the frame is treated as a normal standalone image, not a video).
///
/// The leading Vivaldi logo glyph and trailing window-control glyphs that OCR may pick up are
/// harmless: the logo is identical across every frame of the session and the buttons sit *after* the
/// marker we cut at, so neither perturbs grouping.
pub fn clean_title(raw: &str) -> Option<String> {
    let pos = VIDEO_MARKERS.iter().find_map(|marker| find_ci(raw, marker))?;
    let title = collapse_whitespace(&raw[..pos]);
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

/// The grouping key for a cleaned title: lowercased, punctuation-insensitive, whitespace-collapsed,
/// so trivial OCR jitter (a stray comma, casing) still lands two frames of one video in one group.
pub fn group_key(title: &str) -> String {
    let mut key = String::with_capacity(title.len());
    let mut last_space = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                key.push(lower);
            }
            last_space = false;
        } else if !last_space {
            key.push(' ');
            last_space = true;
        }
    }
    key.trim().to_string()
}

/// Deterministically picks up to `n` of `members`. With `members.len() <= n` it returns them all;
/// otherwise it shuffles a copy with an FNV-seeded xorshift (seed derived from the member hashes, so
/// the choice is stable across runs) and takes the first `n`, returned in sorted order for a tidy
/// plan file.
pub fn sample_hashes(members: &[String], n: usize) -> Vec<String> {
    let mut sorted: Vec<String> = members.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.len() <= n {
        return sorted;
    }

    // FNV-1a over every member hash → a seed that only depends on the group's contents.
    let mut seed: u64 = 0xcbf29ce484222325;
    for hash in &sorted {
        for byte in hash.bytes() {
            seed ^= byte as u64;
            seed = seed.wrapping_mul(0x100000001b3);
        }
    }
    let mut rng = seed | 1; // xorshift must never be seeded with 0

    let mut indices: Vec<usize> = (0..sorted.len()).collect();
    for i in (1..indices.len()).rev() {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let j = (rng % (i as u64 + 1)) as usize;
        indices.swap(i, j);
    }
    let mut chosen: Vec<String> = indices.into_iter().take(n).map(|i| sorted[i].clone()).collect();
    chosen.sort();
    chosen
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkGroup {
    /// A representative display title for the group (verbatim casing from OCR).
    pub title: String,
    /// Every image hash whose title-strip resolved to this group.
    pub member_hashes: Vec<String>,
    /// The (up to `samples_per_group`) hashes chosen for the vision pass.
    pub selected_hashes: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkPlan {
    pub version: u32,
    pub generated_at: String,
    pub samples_per_group: u32,
    pub groups: Vec<ChunkGroup>,
}

/// Builds (or rebuilds) a chunk plan from `(hash, cleaned_title)` pairs — one entry per image whose
/// title-strip resolved to a video title.
///
/// When `previous` is supplied and `force` is false, a group that still exists keeps its earlier
/// selection (intersected with its current members) instead of re-sampling, so a scan that merely
/// adds new frames never reshuffles the frozen set you already reviewed. `force` re-samples every
/// group from scratch.
pub fn build_plan(
    titled: &[(String, String)],
    samples_per_group: u32,
    generated_at: String,
    previous: Option<&ChunkPlan>,
    force: bool,
) -> ChunkPlan {
    let n = samples_per_group.max(1) as usize;

    // key -> (display title, member hashes)
    let mut grouped: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for (hash, title) in titled {
        let key = group_key(title);
        if key.is_empty() {
            continue;
        }
        let entry = grouped.entry(key).or_insert_with(|| (title.clone(), Vec::new()));
        entry.1.push(hash.clone());
    }

    // Previous selections, keyed the same way, so we can preserve frozen picks across a rescan.
    let prior: BTreeMap<String, Vec<String>> = previous
        .map(|plan| {
            plan.groups
                .iter()
                .map(|g| (group_key(&g.title), g.selected_hashes.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut groups: Vec<ChunkGroup> = grouped
        .into_iter()
        .map(|(key, (title, mut members))| {
            members.sort();
            members.dedup();
            let member_set: std::collections::HashSet<&String> = members.iter().collect();

            let selected = if !force {
                if let Some(previous_selection) = prior.get(&key) {
                    let kept: Vec<String> = previous_selection
                        .iter()
                        .filter(|hash| member_set.contains(hash))
                        .cloned()
                        .collect();
                    if !kept.is_empty() {
                        Some(kept)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let selected_hashes = selected.unwrap_or_else(|| sample_hashes(&members, n));
            ChunkGroup {
                title,
                member_hashes: members,
                selected_hashes,
            }
        })
        .collect();

    // Biggest groups first — those are where dedup saves the most.
    groups.sort_by(|a, b| b.member_hashes.len().cmp(&a.member_hashes.len()).then_with(|| a.title.cmp(&b.title)));

    ChunkPlan {
        version: 1,
        generated_at,
        samples_per_group,
        groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_title_strips_youtube_marker_and_keeps_badge_text() {
        assert_eq!(
            clean_title("Driving across the Pyrenees mountains from France FR to Andorra AD - YouTube").as_deref(),
            Some("Driving across the Pyrenees mountains from France FR to Andorra AD")
        );
    }

    #[test]
    fn clean_title_handles_en_dash_and_collapses_whitespace() {
        assert_eq!(clean_title("My Road Trip   – YouTube").as_deref(), Some("My Road Trip"));
    }

    #[test]
    fn clean_title_returns_none_without_a_marker() {
        assert_eq!(clean_title("Just a desktop screenshot"), None);
        assert_eq!(clean_title(""), None);
        assert_eq!(clean_title(" - YouTube"), None); // marker only, empty title
    }

    #[test]
    fn clean_title_keeps_leading_logo_noise() {
        // A stray leading Vivaldi-logo glyph stays in the title; it is identical on every frame of a
        // session, so it does not split a group even though it isn't stripped.
        assert_eq!(clean_title("V Road Trip - YouTube").as_deref(), Some("V Road Trip"));
    }

    #[test]
    fn group_key_ignores_punctuation_and_case() {
        assert_eq!(group_key("France, FR to Andorra"), group_key("france fr  to andorra"));
    }

    #[test]
    fn group_key_differs_when_a_badge_is_dropped() {
        // Documents the known v1 limitation: if OCR drops a country badge on some frames, those
        // frames key differently and split off. The hand-editable plan file is the escape hatch.
        assert_ne!(group_key("France FR to Andorra"), group_key("France to Andorra"));
    }

    #[test]
    fn sample_hashes_returns_all_when_at_or_under_n() {
        let members = vec!["b".to_string(), "a".to_string(), "a".to_string()];
        assert_eq!(sample_hashes(&members, 10), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn sample_hashes_is_deterministic_bounded_and_a_subset() {
        let members: Vec<String> = (0..25).map(|i| format!("hash{i:02}")).collect();
        let first = sample_hashes(&members, 10);
        let second = sample_hashes(&members, 10);
        assert_eq!(first, second, "same input must yield the same sample");
        assert_eq!(first.len(), 10);
        for hash in &first {
            assert!(members.contains(hash));
        }
    }

    #[test]
    fn sample_hashes_ignores_input_order() {
        let forward: Vec<String> = (0..25).map(|i| format!("hash{i:02}")).collect();
        let mut reversed = forward.clone();
        reversed.reverse();
        assert_eq!(sample_hashes(&forward, 10), sample_hashes(&reversed, 10));
    }

    fn frames(title: &str, count: usize) -> Vec<(String, String)> {
        (0..count).map(|i| (format!("{title}-h{i:02}"), title.to_string())).collect()
    }

    fn sorted_members(titled: &[(String, String)]) -> Vec<String> {
        let mut members: Vec<String> = titled.iter().map(|(hash, _)| hash.clone()).collect();
        members.sort();
        members
    }

    #[test]
    fn build_plan_groups_frames_by_title_and_caps_the_sample() {
        let mut titled = frames("Trip A", 15);
        titled.extend(frames("Trip B", 3));
        let plan = build_plan(&titled, 10, "t".into(), None, false);

        assert_eq!(plan.groups.len(), 2);
        let a = plan.groups.iter().find(|g| g.title == "Trip A").unwrap();
        assert_eq!(a.member_hashes.len(), 15);
        assert_eq!(a.selected_hashes.len(), 10); // capped at N
        let b = plan.groups.iter().find(|g| g.title == "Trip B").unwrap();
        assert_eq!(b.member_hashes.len(), 3);
        assert_eq!(b.selected_hashes.len(), 3); // fewer than N -> keep all
    }

    #[test]
    fn build_plan_preserves_frozen_selection_but_force_resamples() {
        let titled = frames("Trip A", 15);
        let members = sorted_members(&titled);
        let hand_picked: Vec<String> = members.iter().take(10).cloned().collect();
        let fresh = sample_hashes(&members, 10);
        assert_ne!(hand_picked, fresh, "test is only meaningful if the two selections differ");

        let previous = ChunkPlan {
            version: 1,
            generated_at: "t".into(),
            samples_per_group: 10,
            groups: vec![ChunkGroup {
                title: "Trip A".into(),
                member_hashes: members.clone(),
                selected_hashes: hand_picked.clone(),
            }],
        };

        let kept = build_plan(&titled, 10, "t".into(), Some(&previous), false);
        assert_eq!(kept.groups[0].selected_hashes, hand_picked, "non-force must preserve the frozen pick");

        let forced = build_plan(&titled, 10, "t".into(), Some(&previous), true);
        assert_eq!(forced.groups[0].selected_hashes, fresh, "force must re-sample");
    }

    #[test]
    fn build_plan_drops_stale_hashes_from_a_preserved_selection() {
        let titled = frames("Trip A", 12);
        let members = sorted_members(&titled);
        let mut stale = members.iter().take(5).cloned().collect::<Vec<_>>();
        stale.push("no-longer-a-member".into());

        let previous = ChunkPlan {
            version: 1,
            generated_at: "t".into(),
            samples_per_group: 10,
            groups: vec![ChunkGroup {
                title: "Trip A".into(),
                member_hashes: members.clone(),
                selected_hashes: stale,
            }],
        };

        let plan = build_plan(&titled, 10, "t".into(), Some(&previous), false);
        assert_eq!(plan.groups[0].selected_hashes, members.iter().take(5).cloned().collect::<Vec<_>>());
    }

    #[test]
    fn chunk_plan_round_trips_through_json() {
        let titled = frames("Trip A", 12);
        let plan = build_plan(&titled, 10, "2026-01-01".into(), None, false);
        let json = serde_json::to_string(&plan).unwrap();
        let back: ChunkPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.groups.len(), plan.groups.len());
        assert_eq!(back.groups[0].selected_hashes, plan.groups[0].selected_hashes);
    }
}
