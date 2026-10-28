//! Mosaic recognition: an image made of smaller images — a screenshot of Super Image Viewer's or
//! this app's own grid, an image search, a YouTube feed of thumbnails, a collage.
//!
//! Like K-pop's description readings, this is a rule over what the Describe pass already wrote
//! (`desc_cues`), not a new look at pixels, and it is re-derived on every save. The model names
//! these pictures reliably in its opening sentence: "This image is a 4x4 grid collage consisting of
//! sixteen different images…", "…'Super Image Viewer' displaying a grid of various image
//! thumbnails", "…the YouTube homepage… displays several video thumbnails".
//!
//! Measured 2026-10-28 over the live library's 50,967 description leads: 1,742 match — 854 by
//! collage/mosaic, 252 by grid, 31 by image search, 605 by a quantity of images (almost all YouTube
//! feeds). Two shapes are deliberately NOT a mosaic, and both turned up in the bare "two images"
//! sense the quantity rule would otherwise take: a split screen of two scenes, and a page that
//! merely carries a couple of pictures ("a Wikipedia article… includes two images"). Hence a count
//! has to be four or more, and "panels"/"scenes" never count — game UIs are full of panels.
//!
//! A capture of the whole desktop is never a mosaic, whatever its description says: the model calls
//! a multi-monitor desktop of windows "a collage of various software screenshots", and that took
//! 361 of the library's 452 full-screen captures before this rule (1,351 mosaics remain, with
//! the hand-sorted ones left where they are). screenshot-tool tags those
//! files `_full-screen`. The same wording on a window or region capture is rare (16) and mostly a
//! real collage, so it stays.
//!
//! What it cannot see: an image Describe has not reached yet (a new screenshot waits for the next
//! nightly run) or will not look at (unsampled video frames, Explicit, excluded categories).

use std::sync::OnceLock;

use regex::Regex;

pub const MOSAIC_CATEGORY: &str = "Mosaic";

/// `classified_by` for an image this module put in [`MOSAIC_CATEGORY`].
pub const CLASSIFIED_BY_MOSAIC: &str = "auto-mosaic";

/// Small counts, for "4x4" / "four-by-four".
const SIDE: &str = r"(?:\d+|two|three|four|five|six)";
/// Four or more — fewer is a split screen or a page with a picture or two, not a mosaic.
const MANY_COUNT: &str =
    r"(?:[4-9]|[1-9]\d+|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|twenty|thirty|dozens of)";
const MANY: &str =
    r"(?:multiple|several|many|numerous|various|a series of|a collection of|an array of|a variety of)";
const PICTURES: &str = r"(?:images|photos|photographs|pictures|thumbnails|screenshots|stills)";

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let rules = [
            r"\bcollage\b|\bmosaic\b|\bcontact sheet\b".to_string(),
            format!(r"\b{SIDE}\s*(?:x|×|-by-)\s*{SIDE}\s+grid\b"),
            format!(r"\bgrid (?:layout )?of (?:[\w'-]+\s+){{0,3}}?(?:{PICTURES}|frames|tiles)\b"),
            format!(r"\b{PICTURES} (?:arranged|laid out|organi[sz]ed) in (?:a )?(?:grid|rows)\b"),
            r"\b(?:image search results|google images)\b".to_string(),
            format!(r"\b(?:{MANY}|{MANY_COUNT})\s+(?:[\w'-]+\s+){{0,2}}?{PICTURES}\b"),
        ];
        Regex::new(&format!("(?i){}", rules.join("|"))).expect("mosaic regex")
    })
}

/// Whether a description's opening says the picture is made of smaller pictures.
pub fn describes_mosaic(lead: &str) -> bool {
    pattern().is_match(lead)
}

/// Whether `path` is a screenshot-tool capture of the whole desktop
/// (`screenshot_<date>_<time>_<ms>_full-screen.png`, or `…_full-screen_1.png` on a collision).
pub fn is_whole_desktop(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.starts_with("screenshot_") && name.contains("_full-screen")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grids_collages_and_feeds_are_mosaics() {
        for lead in [
            "This image is a 4x4 grid collage consisting of sixteen different images including video game screenshots.",
            "This is a collage of sixteen different images arranged in a four-by-four grid.",
            "This is a screenshot of a desktop application called \"Super Image Viewer\" displaying a grid of various image thumbnails.",
            "This is a screenshot of the YouTube homepage in a web browser. The image displays several video thumbnails.",
            "This screenshot displays a numbered grid of 30 photographs showing various data center buildings.",
            "Google Images results for red bridges.",
            "This is a screenshot of a YouTube video featuring a 3x3 grid of nine individual video frames.",
        ] {
            assert!(describes_mosaic(lead), "{lead}");
        }
    }

    #[test]
    fn a_picture_or_two_and_panels_are_not() {
        for lead in [
            "This is a screenshot of a YouTube video featuring two distinct scenes.",
            "This is a screenshot of a Wikipedia article titled \"Tushita.\" The page includes two images on the right side.",
            "This image consists of three side-by-side video stills showing cars driving on a street.",
            "This is a screenshot of a game user interface featuring several panels with bright green borders.",
            "This is a screenshot of a YouTube video showing a quiet street with buildings featuring tiled roofs.",
            "This is a video still from a music video featuring four women dancing on a rooftop.",
        ] {
            assert!(!describes_mosaic(lead), "{lead}");
        }
    }

    #[test]
    fn whole_desktop_captures_are_told_apart_by_name() {
        assert!(is_whole_desktop("Screenshots 10-2026/screenshot_20261026_193512_101_full-screen.png"));
        assert!(is_whole_desktop("Screenshots 10-2026/screenshot_20261026_193512_101_full-screen_1.png"));
        assert!(!is_whole_desktop("Screenshots 10-2026/screenshot_20261026_193512_101_window.png"));
        assert!(!is_whole_desktop("Screenshots 10-2026/screenshot_20261026_193512_101_region.png"));
        assert!(!is_whole_desktop("kela/full-screen collage.png"));
    }
}
