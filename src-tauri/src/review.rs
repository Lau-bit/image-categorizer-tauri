//! Post-build review of country sets — the one place to see what is wrong with sets that already
//! exist, and fix it.
//!
//! The rest of the geo layer is a *forward* pipeline: describe, derive, classify, build. Everything
//! it gets wrong lands in a built set, and until now the only way to find that was to open sets in
//! the viewer one at a time and right-click the bad ones. This module walks the built sets once and
//! reports every member it can argue against, grouped by the reason, with the fix already filled in.
//!
//! **It reviews what is on disk, not what a fresh derive would produce.** That is the point: sets
//! are built once and then looked at for weeks, so they outlive the corrections made after them.
//! A finding therefore compares a set against the *current* records, gazetteer and scene kinds —
//! which is also why "this set predates a correction" is itself one of the findings.
//!
//! Every finding carries its own fix, and all the fixes are writes to the two files that already
//! own this kind of judgement: the exclusion list (`geo::GEO_EXCLUDED_FILE_NAME`) and the gazetteer
//! (`overrides` / `fictionTitlePatterns`). Nothing here invents a fourth place to store decisions,
//! and nothing here edits the derived records — apply a fix, re-derive, rebuild, and the correction
//! is permanent because the input changed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::geo::{self, GeoFile, GeoSet, GeoSetsFile};

/// A country whose whole existence rests on this many videos or fewer is not a country you can
/// practise — it is one video with a caption. Worth surfacing even though the set is well-formed.
const SINGLE_SOURCE_CEILING: usize = 2;

/// A set this far under its target size is reported as thin. Sets legitimately come up short when a
/// country has little material; only a big shortfall is worth a line.
const SHORT_SET_FRACTION: f64 = 0.5;

/// Phrases that mark a frame as a picture of people rather than of a place, precise enough to act
/// on. Measured against the live library: twelve set members, all of them music-video or portrait
/// frames the scene classifier had labelled `outdoor` because a street was visible behind the
/// subject. Broader cues were tried and rejected — "watermark" and "text overlay" match legitimate
/// driving footage with a channel bug in the corner far more often than they match a reposted clip.
const PEOPLE_CUES: &[&str] = &[
    "music video",
    "posing",
    "selfie",
    "talking head",
    "close-up of a woman",
    "close-up of a man",
    "close up of a woman",
    "close up of a man",
];

// ---------------------------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewImage {
    pub hash: String,
    /// Relative path inside the library, when the description index still knows it.
    pub path: String,
    /// The location line this image's country came from.
    pub raw: String,
    /// The video it came from, for a propagated tag.
    pub via: String,
    pub kind: String,
}

/// What ticking a finding will actually write. All three lists are unioned across the selected
/// findings before anything is written, so overlapping suggestions cost one write, not three.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFix {
    /// Human-readable summary of the write, shown next to the checkbox.
    pub label: String,
    pub exclude_hashes: Vec<String>,
    /// Location strings to map to `null` in the gazetteer — rejected as non-geographic.
    pub reject_locations: Vec<String>,
    /// Video titles to add to the fiction denylist.
    pub fiction_titles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFinding {
    /// Stable across re-runs, so a selection survives a refresh of the panel.
    pub id: String,
    pub kind: String,
    /// `high` = the set is teaching something false. `medium` = a member that is not a place.
    /// `low` = worth knowing, not wrong.
    pub severity: String,
    pub country: String,
    pub set_id: String,
    pub set_title: String,
    /// One line naming the problem.
    pub title: String,
    /// Why it is a problem, in the terms the geo layer thinks in.
    pub detail: String,
    pub images: Vec<ReviewImage>,
    pub fix: Option<ReviewFix>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetReview {
    pub generated_at: String,
    pub sets_reviewed: usize,
    pub members_reviewed: usize,
    /// True when the sets on disk were built against a different gazetteer than the records were
    /// derived with — every other finding is then a review of stale material.
    pub stale: bool,
    pub stale_detail: String,
    pub findings: Vec<ReviewFinding>,
    pub counts: BTreeMap<String, usize>,
}

/// The selected fixes, unioned by the frontend and applied in one pass.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewApply {
    #[serde(default)]
    pub exclude_hashes: Vec<String>,
    #[serde(default)]
    pub reject_locations: Vec<String>,
    #[serde(default)]
    pub fiction_titles: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewApplied {
    pub excluded: usize,
    pub rejected: usize,
    pub fiction: usize,
}

// ---------------------------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------------------------

/// Everything the review needs, gathered by the caller so this stays a pure function.
pub struct ReviewInput<'a> {
    pub geo: &'a GeoFile,
    pub sets: &'a GeoSetsFile,
    /// hash -> scene kind (own + inherited), exactly what set building filtered against.
    pub kinds: &'a BTreeMap<String, String>,
    pub allowed_kinds: &'a [String],
    /// Chunk-plan group titles, indexed the same way `GeoRecord::source_group` is.
    pub group_titles: &'a [String],
    /// hash -> description prose, for set members only.
    pub descriptions: &'a HashMap<String, String>,
    /// hash -> relative path, for showing the user which file a finding is about.
    pub paths: &'a HashMap<String, String>,
}

/// "1 member" / "3 members". Findings are read as sentences, and "1 members" reads as a bug.
fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn review_image(
    hash: &str,
    input: &ReviewInput<'_>,
) -> ReviewImage {
    let record = input.geo.images.get(hash);
    ReviewImage {
        hash: hash.to_string(),
        path: input.paths.get(hash).cloned().unwrap_or_default(),
        raw: record.map(|record| record.raw.clone()).unwrap_or_default(),
        via: record
            .and_then(|record| record.via.clone())
            .unwrap_or_default(),
        kind: input.kinds.get(hash).cloned().unwrap_or_default(),
    }
}

/// Walks the built sets and reports everything arguable about them.
pub fn review(input: &ReviewInput<'_>, now: String) -> SetReview {
    let mut findings: Vec<ReviewFinding> = Vec::new();

    // Records store the plan group a frame came from, and on sets built before source merging that
    // is a *title reading*, not a video. Re-deriving the canonical map here rather than trusting the
    // stored index is what lets the review find duplicate videos in old sets at all.
    let titles: Vec<&str> = input.group_titles.iter().map(String::as_str).collect();
    let canonical = geo::canonical_groups(&titles);
    let video_of = |hash: &str| -> String {
        match input.geo.images.get(hash).and_then(|record| record.source_group) {
            Some(index) => format!("v{}", canonical.get(index).copied().unwrap_or(index)),
            None => format!("s{hash}"),
        }
    };

    let stale = input
        .sets
        .sets
        .iter()
        .any(|set| set.gazetteer_fingerprint != input.geo.gazetteer_fingerprint);
    let stale_detail = if stale {
        "These sets were built against an older gazetteer than the records were derived with. \
         Rebuild Country Sets before working the list, or you will be fixing sets that are about \
         to be replaced."
            .to_string()
    } else {
        String::new()
    };

    let mut members_reviewed = 0usize;
    // Country -> total distinct videos across its sets, for the single-video check.
    let mut videos_per_country: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for set in &input.sets.sets {
        members_reviewed += set.members.len();
        for hash in &set.members {
            videos_per_country
                .entry(set.country.clone())
                .or_default()
                .insert(video_of(hash));
        }

        findings.extend(duplicate_video_finding(set, input, &video_of));
        findings.extend(registry_port_finding(set, input));
        findings.extend(not_a_place_findings(set, input));
        findings.extend(short_set_finding(set, input));
    }

    findings.extend(single_video_country_findings(input, &videos_per_country));

    // Worst first, then by country so a country's problems read together.
    let rank = |severity: &str| match severity {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    };
    findings.sort_by(|a, b| {
        rank(&a.severity)
            .cmp(&rank(&b.severity))
            .then_with(|| a.country.cmp(&b.country))
            .then_with(|| a.kind.cmp(&b.kind))
    });

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for finding in &findings {
        *counts.entry(finding.kind.clone()).or_insert(0) += 1;
    }

    SetReview {
        generated_at: now,
        sets_reviewed: input.sets.sets.len(),
        members_reviewed,
        stale,
        stale_detail,
        findings,
        counts,
    }
}

/// Members that are frames of the same video, despite the set claiming they are separate sources.
///
/// This is what a set looked like when one title read sixteen different ways: sixteen "videos",
/// five of them the same train ride. Newly built sets should never trip it — the derive merges
/// sources now — so a hit here means the set predates that and should be rebuilt.
fn duplicate_video_finding(
    set: &GeoSet,
    input: &ReviewInput<'_>,
    video_of: &impl Fn(&str) -> String,
) -> Vec<ReviewFinding> {
    let mut by_video: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for hash in &set.members {
        by_video.entry(video_of(hash)).or_default().push(hash.clone());
    }
    let distinct_videos = by_video.len();
    let duplicates: Vec<(String, Vec<String>)> = by_video
        .into_iter()
        .filter(|(_video, hashes)| hashes.len() > 1)
        .collect();
    if duplicates.is_empty() {
        return Vec::new();
    }

    // Keep the first frame of each video, drop the rest: the video still belongs in the set, it
    // just does not get to be several of its sixteen slots.
    let redundant: Vec<String> = duplicates
        .iter()
        .flat_map(|(_video, hashes)| hashes.iter().skip(1).cloned())
        .collect();
    let images: Vec<ReviewImage> = redundant
        .iter()
        .map(|hash| review_image(hash, input))
        .collect();

    vec![ReviewFinding {
        id: format!("duplicate-video:{}", set.id),
        kind: "duplicate-video".to_string(),
        severity: "high".to_string(),
        country: set.country.clone(),
        set_id: set.id.clone(),
        set_title: set.title.clone(),
        title: format!(
            "{} of {} slots are repeat frames of a video already in the set",
            redundant.len(),
            set.size
        ),
        detail: format!(
            "The set reports {} videos but draws on only {}. A set is meant to be one frame per \
             video — repeats teach one road rather than a country, which is the false prior the \
             whole design exists to avoid. Rebuilding Country Sets fixes this at the source; \
             excluding the repeats fixes this set.",
            set.sources,
            // Saturating, not subtracting: a set built one-frame-per-*title-reading* can hold more
            // repeats than it ever claimed videos, and an underflow here aborts the whole app.
            set.sources.saturating_sub(redundant.len()).max(distinct_videos)
        ),
        images,
        fix: Some(ReviewFix {
            label: format!("Exclude {}", plural(redundant.len(), "repeat frame", "repeat frames")),
            exclude_hashes: redundant,
            reject_locations: Vec::new(),
            fiction_titles: Vec::new(),
        }),
    }]
}

/// Members whose country came off a ship's stern rather than out of the scenery.
fn registry_port_finding(set: &GeoSet, input: &ReviewInput<'_>) -> Vec<ReviewFinding> {
    let mut hits: Vec<String> = Vec::new();
    let mut excludable: Vec<String> = Vec::new();
    let mut locations: BTreeSet<String> = BTreeSet::new();
    for hash in &set.members {
        let Some(record) = input.geo.images.get(hash) else {
            continue;
        };
        let Some(description) = input.descriptions.get(hash) else {
            continue;
        };
        if !geo::is_registry_port_reading(description, &record.raw) {
            continue;
        }
        hits.push(hash.clone());
        locations.insert(record.raw.to_lowercase());
        // A frame whose video ALSO resolved to a real country is a good image with one bad tag —
        // a ship registered in Panama filmed on a Vietnamese river belongs in Vietnam. Rejecting
        // the string drops the wrong country on the next derive; excluding the image would throw
        // away the right one too.
        if record.countries.len() == 1 {
            excludable.push(hash.clone());
        }
    }
    if hits.is_empty() {
        return Vec::new();
    }
    let images: Vec<ReviewImage> = hits.iter().map(|hash| review_image(hash, input)).collect();
    let label = if excludable.is_empty() {
        format!(
            "Reject {} — the images belong to their other country",
            plural(locations.len(), "location string", "location strings")
        )
    } else {
        format!(
            "Exclude {} and reject the location string",
            plural(excludable.len(), "member", "members")
        )
    };
    vec![ReviewFinding {
        id: format!("registry-port:{}", set.id),
        kind: "registry-port".to_string(),
        severity: "high".to_string(),
        country: set.country.clone(),
        set_id: set.id.clone(),
        set_title: set.title.clone(),
        title: format!(
            "{} tagged from a ship's port of registry",
            plural(hits.len(), "member", "members")
        ),
        detail: format!(
            "The location line is the flag painted on a vessel ({}), not where the footage was \
             shot — a container ship registered in {} filmed anywhere in the world reads as {}. \
             Re-deriving removes these automatically; the fix here also rejects the location \
             string outright so nothing else can pick it up.",
            locations.iter().cloned().collect::<Vec<_>>().join(", "),
            set.country,
            set.country
        ),
        images,
        fix: Some(ReviewFix {
            label,
            exclude_hashes: excludable,
            reject_locations: locations.into_iter().collect(),
            fiction_titles: Vec::new(),
        }),
    }]
}

/// Members that carry a real country but do not show a place: an unclassified frame that slipped
/// past the scene filter, or one the classifier called outdoor while its own description describes
/// people.
fn not_a_place_findings(set: &GeoSet, input: &ReviewInput<'_>) -> Vec<ReviewFinding> {
    let allowed: BTreeSet<&str> = input.allowed_kinds.iter().map(String::as_str).collect();
    let mut wrong_kind: Vec<String> = Vec::new();
    let mut unclassified: Vec<String> = Vec::new();
    let mut people: Vec<String> = Vec::new();

    for hash in &set.members {
        match input.kinds.get(hash) {
            // Set building lets an unclassified image through on purpose — an optional pass that
            // has not run must not empty every set — so this is the hole that leaves open.
            None => unclassified.push(hash.clone()),
            Some(kind) if !allowed.contains(kind.as_str()) => wrong_kind.push(hash.clone()),
            Some(_) => {
                if let Some(description) = input.descriptions.get(hash) {
                    let lowered = description.to_lowercase();
                    if PEOPLE_CUES.iter().any(|cue| lowered.contains(cue)) {
                        people.push(hash.clone());
                    }
                }
            }
        }
    }

    let mut findings = Vec::new();
    if !wrong_kind.is_empty() {
        let images = wrong_kind.iter().map(|hash| review_image(hash, input)).collect();
        findings.push(ReviewFinding {
            id: format!("wrong-kind:{}", set.id),
            kind: "wrong-kind".to_string(),
            severity: "medium".to_string(),
            country: set.country.clone(),
            set_id: set.id.clone(),
            set_title: set.title.clone(),
            title: format!(
                "{} of a scene kind sets are not allowed to use",
                plural(wrong_kind.len(), "member is", "members are")
            ),
            detail: "These were classified after the set was built, or the allowed-kinds list \
                     changed since. Rebuilding Country Sets drops them without excluding anything."
                .to_string(),
            images,
            fix: Some(ReviewFix {
                label: format!("Exclude {}", plural(wrong_kind.len(), "member", "members")),
                exclude_hashes: wrong_kind,
                reject_locations: Vec::new(),
                fiction_titles: Vec::new(),
            }),
        });
    }
    if !unclassified.is_empty() {
        let images = unclassified.iter().map(|hash| review_image(hash, input)).collect();
        findings.push(ReviewFinding {
            id: format!("unclassified:{}", set.id),
            kind: "unclassified".to_string(),
            severity: "low".to_string(),
            country: set.country.clone(),
            set_id: set.id.clone(),
            set_title: set.title.clone(),
            title: format!(
                "{} never scene-classified",
                plural(unclassified.len(), "member was", "members were")
            ),
            detail: "An image with no scene kind passes the filter unchecked, so nothing has \
                     confirmed these show a place at all. Run Classify Scenes, then rebuild."
                .to_string(),
            images,
            fix: None,
        });
    }
    if !people.is_empty() {
        let images = people.iter().map(|hash| review_image(hash, input)).collect();
        findings.push(ReviewFinding {
            id: format!("people:{}", set.id),
            kind: "people".to_string(),
            severity: "medium".to_string(),
            country: set.country.clone(),
            set_id: set.id.clone(),
            set_title: set.title.clone(),
            title: format!(
                "{} people, not a place",
                plural(people.len(), "member describes", "members describe")
            ),
            detail: "The scene classifier called these outdoor because a street is visible behind \
                     the subject, but the description is of a music video, a portrait or a posed \
                     shot. A country is not learnable from the pavement behind a performer."
                .to_string(),
            images,
            fix: Some(ReviewFix {
                label: format!("Exclude {}", plural(people.len(), "member", "members")),
                exclude_hashes: people,
                reject_locations: Vec::new(),
                fiction_titles: Vec::new(),
            }),
        });
    }
    findings
}

fn short_set_finding(set: &GeoSet, input: &ReviewInput<'_>) -> Vec<ReviewFinding> {
    let target = input.sets.target_size.max(1);
    if (set.size as f64) >= target as f64 * SHORT_SET_FRACTION {
        return Vec::new();
    }
    vec![ReviewFinding {
        id: format!("short-set:{}", set.id),
        kind: "short-set".to_string(),
        severity: "low".to_string(),
        country: set.country.clone(),
        set_id: set.id.clone(),
        set_title: set.title.clone(),
        title: format!("Only {} of {} images", set.size, target),
        detail: "The country ran out of usable frames. Fine as reference, but a board sized to \
                 this set shows mostly empty tiles — capture more of this country or accept it as \
                 a look-at-it set."
            .to_string(),
        images: Vec::new(),
        fix: None,
    }]
}

/// Countries that exist because of one or two videos. Not a malformed set — a country that cannot
/// be practised, and usually a sign the tag itself came from something incidental.
fn single_video_country_findings(
    input: &ReviewInput<'_>,
    videos_per_country: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<ReviewFinding> {
    let mut findings = Vec::new();
    for (country, videos) in videos_per_country {
        if videos.len() > SINGLE_SOURCE_CEILING {
            continue;
        }
        let sets: Vec<&GeoSet> = input
            .sets
            .sets
            .iter()
            .filter(|set| &set.country == country)
            .collect();
        let Some(first) = sets.first() else { continue };
        let hashes: Vec<String> = sets
            .iter()
            .flat_map(|set| set.members.iter().cloned())
            .collect();
        let titles: BTreeSet<String> = hashes
            .iter()
            .filter_map(|hash| input.geo.images.get(hash))
            .filter_map(|record| record.via.clone())
            .collect();
        let images = hashes.iter().take(8).map(|hash| review_image(hash, input)).collect();
        findings.push(ReviewFinding {
            id: format!("single-video-country:{country}"),
            kind: "single-video-country".to_string(),
            severity: "medium".to_string(),
            country: country.clone(),
            set_id: first.id.clone(),
            set_title: first.title.clone(),
            title: format!(
                "{country} rests on {} video{}",
                videos.len(),
                if videos.len() == 1 { "" } else { "s" }
            ),
            detail: format!(
                "Every image filed under {country} came from {}. That is one place, one time of \
                 day, one camera — practising on it trains a false prior, and a country this thin \
                 is often a mis-tag rather than a real gap. Excluding it removes the country until \
                 there is real footage of it.",
                if titles.is_empty() {
                    "a single source".to_string()
                } else {
                    titles.iter().cloned().collect::<Vec<_>>().join(" / ")
                }
            ),
            images,
            fix: Some(ReviewFix {
                label: format!(
                    "Exclude all {} — removes {country} entirely",
                    plural(hashes.len(), "image", "images")
                ),
                exclude_hashes: hashes,
                reject_locations: Vec::new(),
                fiction_titles: Vec::new(),
            }),
        });
    }
    findings
}

// ---------------------------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------------------------

/// Writes the selected fixes into the two files that already own this kind of decision. Additive
/// and idempotent: applying the same selection twice changes nothing the second time, and an
/// override the user wrote by hand is never overwritten by a rejection from here.
pub fn apply(
    root: &std::path::Path,
    request: &ReviewApply,
    now: &str,
) -> Result<ReviewApplied, String> {
    let mut applied = ReviewApplied::default();

    if !request.exclude_hashes.is_empty() {
        let mut excluded = geo::load_excluded(root);
        if excluded.note.is_empty() {
            excluded.note = "Images kept out of geo set building by hand. Delete a line to let one \
                             back in. Written by Image Categorizer's set review and by \
                             super-image-viewer's \"Remove from geo sets\" action; both honour it."
                .to_string();
        }
        excluded.version = geo::GEO_SCHEMA_VERSION;
        for hash in &request.exclude_hashes {
            if excluded.excluded.contains_key(hash) {
                continue;
            }
            excluded.excluded.insert(
                hash.clone(),
                geo::GeoExclusion {
                    name: String::new(),
                    excluded_at: now.to_string(),
                    source: "set-review".to_string(),
                },
            );
            applied.excluded += 1;
        }
        let json = serde_json::to_string_pretty(&excluded)
            .map_err(|error| format!("Failed to serialize the exclusion list: {error}"))?;
        std::fs::write(geo::excluded_path(root), json)
            .map_err(|error| format!("Failed to save the exclusion list: {error}"))?;
    }

    if !request.reject_locations.is_empty() || !request.fiction_titles.is_empty() {
        let mut gazetteer = geo::load_gazetteer(root);
        for location in &request.reject_locations {
            let key = location.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            // A hand-written mapping outranks a rejection from the review: the user has already
            // decided what that string means.
            if let Some(Some(_existing)) = gazetteer.overrides.get(&key) {
                continue;
            }
            if gazetteer.overrides.insert(key, None).is_none() {
                applied.rejected += 1;
            }
        }
        for title in &request.fiction_titles {
            let pattern = title.trim().to_lowercase();
            if pattern.is_empty() || gazetteer.fiction_title_patterns.contains(&pattern) {
                continue;
            }
            gazetteer.fiction_title_patterns.push(pattern);
            applied.fiction += 1;
        }
        geo::save_gazetteer(root, &gazetteer)?;
    }

    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{GeoRecord, GeoSetsFile, GeoStats};

    fn record(country: &str, raw: &str, group: Option<usize>) -> GeoRecord {
        GeoRecord {
            countries: vec![country.to_string()],
            raw: raw.to_string(),
            source: "own".into(),
            via: Some("A video".into()),
            source_group: group,
            confidence: "high".into(),
        }
    }

    fn geo_file(images: BTreeMap<String, GeoRecord>) -> GeoFile {
        GeoFile {
            version: 1,
            generated_at: "now".into(),
            gazetteer_fingerprint: "abc".into(),
            stats: GeoStats::default(),
            unresolved: BTreeMap::new(),
            coverage: BTreeMap::new(),
            previous_coverage: BTreeMap::new(),
            images,
        }
    }

    fn set(id: &str, country: &str, members: Vec<String>, sources: usize) -> GeoSet {
        GeoSet {
            id: id.into(),
            kind: "country".into(),
            country: country.into(),
            title: country.into(),
            size: members.len(),
            sources,
            max_per_source: 1,
            quality: "diverse".into(),
            members,
            gazetteer_fingerprint: "abc".into(),
            generated_at: "now".into(),
        }
    }

    #[test]
    fn repeat_frames_of_one_video_are_reported_even_when_the_plan_split_it() {
        // Two plan groups, one video: exactly the shape OCR jitter produced before source merging.
        let mut images = BTreeMap::new();
        images.insert("h0".to_string(), record("Japan", "Kyoto", Some(0)));
        images.insert("h1".to_string(), record("Japan", "Kyoto", Some(1)));
        images.insert("h2".to_string(), record("Japan", "Osaka", Some(2)));
        let geo = geo_file(images);
        let sets = GeoSetsFile {
            version: 1,
            generated_at: "now".into(),
            target_size: 3,
            sets: vec![set("s1", "Japan", vec!["h0".into(), "h1".into(), "h2".into()], 3)],
        };
        let group_titles = vec![
            "Walking through Kyoto at night in the rain".to_string(),
            "(1) Walking through Kyoto at night in the raln".to_string(),
            "Driving across Osaka bay bridge at sunset".to_string(),
        ];
        let allowed = vec!["outdoor".to_string()];
        let mut kinds = BTreeMap::new();
        for hash in ["h0", "h1", "h2"] {
            kinds.insert(hash.to_string(), "outdoor".to_string());
        }
        let descriptions = HashMap::new();
        let paths = HashMap::new();
        let input = ReviewInput {
            geo: &geo,
            sets: &sets,
            kinds: &kinds,
            allowed_kinds: &allowed,
            group_titles: &group_titles,
            descriptions: &descriptions,
            paths: &paths,
        };
        let review = review(&input, "now".into());
        let finding = review
            .findings
            .iter()
            .find(|finding| finding.kind == "duplicate-video")
            .expect("the two Kyoto readings are one video");
        assert_eq!(finding.images.len(), 1, "one frame stays, the repeat goes");
        assert_eq!(finding.fix.as_ref().unwrap().exclude_hashes, vec!["h1".to_string()]);
    }

    #[test]
    fn a_set_that_is_all_one_video_reviews_instead_of_panicking() {
        // The Liberia shape: sixteen frames, one video, `sources: 1`. Reporting "1 video, 15
        // repeats" used to compute `sources - redundant` in usize and abort the whole app on the
        // underflow — an arithmetic overflow in a report is still a crash.
        let mut images = BTreeMap::new();
        let members: Vec<String> = (0..16)
            .map(|index| {
                let hash = format!("h{index}");
                images.insert(hash.clone(), record("Liberia", "Monrovia", Some(0)));
                hash
            })
            .collect();
        let geo = geo_file(images);
        let sets = GeoSetsFile {
            version: 1,
            generated_at: "now".into(),
            target_size: 16,
            sets: vec![set("s1", "Liberia", members, 1)],
        };
        let kinds = BTreeMap::new();
        let allowed = vec!["outdoor".to_string()];
        let group_titles = vec!["Extreme Shipspotting in a tight river bend".to_string()];
        let descriptions = HashMap::new();
        let paths = HashMap::new();
        let input = ReviewInput {
            geo: &geo,
            sets: &sets,
            kinds: &kinds,
            allowed_kinds: &allowed,
            group_titles: &group_titles,
            descriptions: &descriptions,
            paths: &paths,
        };
        let review = review(&input, "now".into());
        let finding = review
            .findings
            .iter()
            .find(|finding| finding.kind == "duplicate-video")
            .expect("fifteen of the sixteen slots are repeats");
        assert_eq!(finding.fix.as_ref().unwrap().exclude_hashes.len(), 15);
    }

    #[test]
    fn a_ship_registry_tag_is_reported_with_the_string_to_reject() {
        let mut images = BTreeMap::new();
        images.insert("h0".to_string(), record("Liberia", "Monrovia", Some(0)));
        let geo = geo_file(images);
        let sets = GeoSetsFile {
            version: 1,
            generated_at: "now".into(),
            target_size: 1,
            sets: vec![set("s1", "Liberia", vec!["h0".into()], 1)],
        };
        let mut descriptions = HashMap::new();
        descriptions.insert(
            "h0".to_string(),
            "A container ship heels hard into a tight river bend.".to_string(),
        );
        let mut kinds = BTreeMap::new();
        kinds.insert("h0".to_string(), "outdoor".to_string());
        let allowed = vec!["outdoor".to_string()];
        let group_titles = vec!["Extreme Shipspotting".to_string()];
        let paths = HashMap::new();
        let input = ReviewInput {
            geo: &geo,
            sets: &sets,
            kinds: &kinds,
            allowed_kinds: &allowed,
            group_titles: &group_titles,
            descriptions: &descriptions,
            paths: &paths,
        };
        let review = review(&input, "now".into());
        let finding = review
            .findings
            .iter()
            .find(|finding| finding.kind == "registry-port")
            .expect("a bare port of registry over a ship is not a location");
        let fix = finding.fix.as_ref().unwrap();
        assert_eq!(fix.exclude_hashes, vec!["h0".to_string()]);
        assert_eq!(fix.reject_locations, vec!["monrovia".to_string()]);
        // The same country also trips the one-video rule, which is the other half of the story.
        assert!(review.findings.iter().any(|f| f.kind == "single-video-country"));
    }

    #[test]
    fn applying_is_additive_and_never_overwrites_a_hand_written_override() {
        let dir = std::env::temp_dir().join(format!("icat-review-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut gazetteer = geo::Gazetteer::default();
        gazetteer
            .overrides
            .insert("panama".into(), Some("Panama".into()));
        geo::save_gazetteer(&dir, &gazetteer).unwrap();

        let request = ReviewApply {
            exclude_hashes: vec!["h0".into(), "h0".into()],
            reject_locations: vec!["panama".into(), "monrovia".into()],
            fiction_titles: vec!["exploring empty maps".into()],
        };
        let first = apply(&dir, &request, "now").unwrap();
        assert_eq!(first.excluded, 1, "a repeated hash is one exclusion");
        assert_eq!(first.rejected, 1, "the hand-written Panama mapping is left alone");
        assert_eq!(first.fiction, 1);

        // Idempotent: nothing new the second time.
        let second = apply(&dir, &request, "now").unwrap();
        assert_eq!(second.excluded, 0);
        assert_eq!(second.rejected, 0);
        assert_eq!(second.fiction, 0);

        let after = geo::load_gazetteer(&dir);
        assert_eq!(after.overrides.get("panama"), Some(&Some("Panama".to_string())));
        assert_eq!(after.overrides.get("monrovia"), Some(&None));
        std::fs::remove_dir_all(&dir).ok();
    }
}
