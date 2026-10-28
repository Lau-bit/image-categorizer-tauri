//! App and web UI screenshots that OCR counts as Low Text — a chat window that is still empty, an
//! installer, a settings panel, a zoomed-in dev UI. Few words, so the word-count split files them
//! with the driving videos, but the user files them under High Text: all 76 images hand-sorted into
//! High Text (as of 2026-10-28) had too few words to get there on their own.
//!
//! Like Mosaic and K-pop's description readings, this is a rule over the Describe pass's opening
//! sentence (`desc_cues`), re-derived on every save. Its subject has to be a piece of software, and
//! nothing in the opening may say the picture is footage, a game, a 3D scene or a photo — a browser
//! playing a driving video is a video, whatever window it sits in.
//!
//! Measured 2026-10-28 on the live library: it takes 447 images (436 of them Low Text). By eye, about four in five
//! of a 48-image sample were plainly UI (chat apps, settings, web pages, forms, terminals, clock
//! widgets) and the rest were UI windows showing a picture — an asset gallery, a 3D viewer. Of the
//! hand-sorted High Text with a description, 56 of 68 match.

use std::sync::OnceLock;

use regex::Regex;

/// `classified_by` for a Low-Text-by-OCR image this rule filed under High Text.
pub const CLASSIFIED_BY_UI: &str = "auto-ui";

fn subject() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r#"(?i)^(?:this is |this image is |this screenshot (?:shows|displays) |a |the )?(?:a |an |the )?"#,
            r#"(?:screenshot|screen capture|close-up|view)? ?(?:of |from |showing )?(?:a |an |the )?"#,
            r#"(?:[\w'".-]+ ){0,4}?"#,
            r"(?:software|application|app|interface|user interface|ui|web browser|browser|webpage|web page|website|",
            r"code editor|terminal|command prompt|console|dashboard|settings|dialog|dialogue|installer|chat|",
            r"file explorer|spreadsheet|document|text editor|form|menu|login|error page|new tab page|calculator|widget|panel)\b",
        ))
        .expect("ui subject regex")
    })
}

fn picture() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)\b(?:video still|still from|youtube video|video player|playing a video|video game|game|gameplay|3d|",
            r"render|rendered|scene|photograph|photo|artwork|illustration|map|maps|satellite|street view|",
            r"music video|dashcam|driving|footage|camera|movie|film|trailer|livestream|live stream)\b",
        ))
        .expect("ui picture regex")
    })
}

/// Any mention of video, and the software that is ABOUT video rather than showing one. A browser
/// "playing a video", "displaying a video of a marina", a "video interface" are footage in a window
/// (12 of the first 16 UI matches that mentioned video, ARTMS's "Blue Blood" among them); a video
/// editor, a generator, a chat with a video message are software. No lookahead in this regex
/// crate, hence two patterns.
fn video() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bvideos?\b").expect("video regex"))
}

fn video_tool() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\bvideos?\s+(?:editing|editor|generation|generator|message|call|conference|chat)\b")
            .expect("video tool regex")
    })
}

/// Whether a description's opening says the picture is a piece of software rather than a picture.
pub fn describes_ui(lead: &str) -> bool {
    let first = lead.split(". ").next().unwrap_or(lead);
    subject().is_match(first)
        && !picture().is_match(lead)
        && !(video().is_match(lead) && !video_tool().is_match(lead))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_and_web_pages_are_ui() {
        for lead in [
            "This is a screenshot of a software installation window for Ollama version 0.30.10.",
            "This is a screenshot of the Grok Chat application interface. The window features a dark theme.",
            "This is a screenshot of a web browser's new tab page featuring a dark background.",
            "This is a screenshot of the VSCodium code editor showing three side-by-side terminal panes.",
            "This is a screenshot of a calculator application titled \"Nelilaskin\".",
            "This is a screenshot of an AI image generator application displaying a generated image.",
            "This is a screenshot of a video editing application interface.",
            "This is a screenshot of a WhatsApp chat window featuring a shared video message.",
        ] {
            assert!(describes_ui(lead), "{lead}");
        }
    }

    #[test]
    fn footage_games_and_scenes_are_not() {
        for lead in [
            "This is a screenshot from a driving video showing the view from a car's dashboard.",
            "This is a screenshot of a 3D application or game showing the interior of a modern highrise apartment.",
            "This is a screenshot of a YouTube video showing heavy forestry machinery.",
            "This is a video still from a music video featuring four women dancing.",
            "This is a screenshot of a web browser displaying Google Maps over Europe.",
            "A photograph of a red bridge over a river.",
            "This is a screenshot of a video playing in a web browser. The image features a person with short hair.",
            "This is a screenshot of a web browser displaying a video of a marina.",
            "This is a screenshot of a video interface featuring a black background.",
        ] {
            assert!(!describes_ui(lead), "{lead}");
        }
    }
}
