//! K-pop recognition from the on-screen video title.
//!
//! The title-strip pass (`ocr::extract_title_strip` + `chunker::clean_title`) already stores the
//! browser title of every video frame — "(2) [MV] (G)I-DLE ((여자)아이들) _ LATATA - YouTube" becomes
//! `video_title`. So this is a rule over text the library already holds, not a new look at pixels.
//! It is re-evaluated on every `reclassify_text_categories`, never stored as a decision: add a name
//! and the next scan picks up the old frames of that group, remove one and they fall back to Low
//! Text.
//!
//! Measured on the live library (2026-10-18, 3,247 distinct titles): the draft rules matched 420
//! titles / 2,528 frames with only three kinds of miss-fire, and every one is handled below — a
//! case-insensitive `TWICE` ("The Village Where the Sun Rises Twice"), member names in a library full
//! of driving videos ("winter", "sakura"), and an "artms" inside a torrent list.
//!
//! Three things make titles readable despite the OCR, and each is a rule here:
//! - **The K-pop labels' naming convention**: `ARTIST 'Song' MV` / `Official MV` / `Performance
//!   Video`. It catches groups that no list knows (84 titles in the live library — CLC, AtHeart,
//!   ifeye, GFRIEND…), and it is what makes an ambiguous name like `IVE` safe.
//! - **`M/V` is routinely misread** as `MIV`, `M,'V`, `MN`, `MIVI`, and a closing quote as `•`.
//! - **Hangul** is read only when the Korean OCR pack is installed (`video_title_ko`). Without it,
//!   `STAYC (스테이씨)` comes out as `STAYC(4E/OlAl)` — the Latin half survives, which is why the
//!   English-only rules still work.

use std::sync::OnceLock;

use regex::Regex;

pub const KPOP_CATEGORY: &str = "K-pop";

/// `classified_by` for an image this module put in [`KPOP_CATEGORY`]. Distinct from the text pass's
/// `auto` so the dashboard can say which pass decided, and so it is never mistaken for a hand sort.
pub const CLASSIFIED_BY_KPOP: &str = "auto-kpop";

/// How far an untitled frame may sit from the titled K-pop frames on BOTH sides of it and still be
/// filled in, in seconds. Measured over the 515 untitled Low Text frames sandwiched between two
/// K-pop frames: median gap 3 s, 404 within 120 s, and only 25 more by 300 s — past that is the
/// long tail where the session has more likely moved on.
pub const NEIGHBOUR_WINDOW_SECS: i64 = 120;

/// Latin names are bounded by "not an ASCII letter or digit" rather than `\b`, because the regex
/// crate's `\b` is Unicode-aware and Hangul counts as a word character: `IVE와 함께` has no `\b`
/// after `IVE`, and that exact title is one the user watches.
const L: &str = r"(?:^|[^A-Za-z0-9])";
const R: &str = r"(?:$|[^A-Za-z0-9])";

/// The `M/V` family as the OCR actually reads it. `MIVI` is `M/V]` with the bracket eaten.
const MV_TOKEN: &str = r"(?:m\s?[/|,'i]{1,2}\s?v(?:i)?|mv|mn)";

/// Group and soloist names distinctive enough to count on their own. Female acts only — the user
/// watches girl groups and female soloists, so the list stays short and nothing has to tell a boy
/// group apart. Lowercase; matched case-insensitively.
const DISTINCT_ARTISTS: &[&str] = &[
    r"\(g\)\s?i-?dle", r"i-dle", r"le\s?sserafim", "aespa", "newjeans", "itzy", "blackpink",
    r"fromis[_ ]?9", "kep1er", "stayc", "nmixx", "illit", "babymonster", "hearts2hearts", "s2it",
    "cosmosy", "meovv", "izna", "apink", "mamamoo", "wjsn", "loona", "artms", r"odd eye circle",
    "viviz", "billlie", "weeekly", "kiiikiii", "choerry", r"kim lip", "heejin", "atheart", "ifeye",
    "gfriend", "rescene", "exid", r"girls'? generation", "snsd", r"f\(x\)", "2ne1", "t-ara",
    r"wonder girls", "sistar", r"girl'?s day", "lovelyz", "gugudan", r"iz\*?one", "everglow",
    r"purple kiss", "lightsum", r"rocket punch", r"cherry bullet", r"tri\.be", "wooah", "h1-key",
    "badvillain", "vcha", "katseye", r"young posse", r"a2o may", r"crayon pop", "taeyeon",
    "chungha", "jisoo", "sunmi", "hyuna", "yena", r"jeon somi", r"kwon eun\s?bi", "yuqi", "chuu",
    r"jo yuri", "seulgi", "ningning", r"won-?young", "hwasa", "moonbyul", "wheein", "kissoflife",
    r"weki meki", "leeseo", r"ahn? yujin",
    // K-pop channels whose name is the whole caption the model or the band picked up.
    r"1the\s?k", r"mbc\s?kpop",
    // tripleS's sub-units. `LOVElution` reads as `LOVEIution` / `LOVE-lution` off the title band.
    r"love-?[il]ution", r"acid angel from asia", r"\(kr\)ystal eyes",
    // IVE's own channel: the `WONYOUNG LOG` vlog loses its first letters to the favicon, so the
    // band reads `NYOUNG LOG`.
    r"(?:wo)?nyoung\s?log",
];

/// Names whose CASING is what makes them a name: `tripleS` is the group and "triples" is a word,
/// `EVOLution` is a sub-unit and "evolution" is everywhere. Matched case-sensitively, and strong on
/// their own — the measured misses were tripleS's `'Dream Dress' Special Visual Clip` (20 frames)
/// and fan edits like `DNA - tripleS but without the ring ring`, neither with a music-video word.
const CASED_ARTISTS: &[&str] = &["tripleS", "EVOLution"];

/// Names that are ordinary words or names somewhere else — "The Village Where the Sun Rises
/// Twice", a nature stream called "NATURE LIVE 4K". Each counts only next to a second signal
/// ([`co_signal`]). Matched CASE-SENSITIVELY, in the casing the acts style themselves: an all-caps
/// `IVE` is the group, a lowercase `ive` inside OCR noise is not.
const AMBIGUOUS_ARTISTS: &[&str] = &[
    "IVE", "TWICE", "Twice", "IU", "CLC", "LIMELIGHT", "KARA", "JENNIE", "Jennie", "LISA", "ROSÉ",
    "Rosé", "BIBI", "QWER", "UNIS", "NATURE",
];

/// Multi-word ambiguous names — a CPR video called "Kiss of Life", a cake called Red Velvet, a
/// dreamcatcher craft tutorial. Two words together are rarely OCR noise, so these match in ANY
/// casing (the title band reads `Kiss OF LIFE`), and like the names above they still need a
/// second signal.
const AMBIGUOUS_PHRASES: &[&str] = &[
    r"red\s?velvet", r"kiss of life", "dreamcatcher", r"oh my girl", r"fifty fifty",
];

/// IVE's channel series — `[IVE ON]`, `[IVE OFF]`, `[IVE LOG]` — the "IVE fan content" the title
/// rules used to miss because nothing in them is a music-video word. Case-sensitive like `IVE`
/// itself; no right boundary, because the closing bracket is routinely read as `I`
/// (`[IVE LOGI 21`).
const IVE_SERIES: &str = r"\[\s*IVE\s+(?:ON|OFF|LOG)|(?:^|[^A-Za-z0-9])IVE\s+LOG";

/// Music formats and show names that mark K-pop by themselves. Lowercase; case-insensitive. The
/// Hangul ones are substrings (no boundary is meaningful inside a Hangul run).
const FORMAT_MARKERS: &[&str] = &[
    // `[MV]`, `[M/V]`, `[MIV]`, `[MVI` (closing bracket read as I).
    r"\[\s*m\s*[/|il]?\s*v\s*[\]il1|]",
    // `[Official M/V]`, `[OffKial MIVI`, `[Off,cial MN]`.
    r"\[\s*off[\w,]*\s+m[\s/|i,']*[vn][\]il1|]?",
    r"k[\s-]?pop",
    r"girl\s?groups?",
    "fancam",
    "inkigayo",
    r"m\s?countdown",
    r"music\s?bank",
    r"music\s?core",
    r"show\s?champion",
    r"dance practice",
    // VKP's monthly rankings: `[VKP] TOP 20 JANUARY`.
    r"\[vkp\]",
    // A music-show broadcast as the networks title it: `WaiT - iii | SBS 260830 방송`.
    r"(?:sbs|mbc|kbs|mnet)\s?\d{6}\s?방송",
];

const HANGUL_MARKERS: &[&str] = &[
    "여자아이돌", "여자 아이돌", "걸그룹", "아이돌", "케이팝", "직캠", "뮤직비디오", "인기가요",
    "음악중심", "뮤직뱅크", "엠카운트다운", "쇼챔피언", "더쇼",
    // Artist names in Hangul. Only the distinctive ones: 아이들 alone is "children", 있지 is "there
    // is", 로제 is a pasta sauce, 지수 is "index", 윈터 is winter, 여자친구 is "girlfriend", 태연 and
    // 화사 are adjectives, 청하 is "to request" — and these are substring matches.
    "아이브", "트와이스", "르세라핌", "에스파", "뉴진스", "블랙핑크", "레드벨벳", "프로미스나인",
    "(여자)아이들", "여자)아이들", "스테이씨", "엔믹스", "아일릿", "베이비몬스터", "키스오브라이프",
    "트리플에스", "하츠투하츠", "아이유", "오마이걸", "에이핑크", "마마무", "우주소녀", "이달의소녀",
    "이달의 소녀", "아르테미스", "피프티피프티", "피프티 피프티", "비비지", "위클리", "드림캐쳐",
    "케플러", "미야오", "이즈나", "하이키", "씨엘씨", "크레용팝", "소녀시대", "티아라", "원더걸스",
    "씨스타", "걸스데이", "러블리즈", "아이즈원", "에버글로우", "퍼플키스", "라잇썸", "유니스",
    "배드빌런", "캣츠아이", "리센느", "앳하트", "이프아이", "권은비", "최예나", "전소미", "장원영",
    "안유진", "오드아이써클",
];

/// A quoted song title followed by a K-pop-only format word: `IVE 'Accendio' MV`,
/// `cLce,qæAl) - 'HELICOPTER' Official Music Video` does NOT qualify on its own (Western labels say
/// "Official Music Video" too) — `MV` / `Performance Video` / `Dance Practice` / `Special Clip` are
/// the label conventions. The closing quote may have been read as `•`, or lost entirely.
fn mv_shape() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r#"(?i)['"‘’“”`´][^'"‘’“”]{{1,45}}['"‘’“”•]?[^'"‘’“”]{{0,25}}?{L}(?:{MV_TOKEN}|{LABEL_FORMATS}){R}"#
        ))
        .expect("mv shape regex")
    })
}

/// The label formats that follow a quoted song title. `Special Visual Clip` is tripleS's.
const LABEL_FORMATS: &str = r"performance\s?(?:video|film)|dance\s?(?:practice|ver)|special\s?(?:visual\s?)?(?:clip|video|film)|visual\s?clip|music\s?visual|track\s?video|choreography\s?video|official\s?dance";

/// The same shape with the OPENING quote lost — the title band routinely eats it along with the
/// artist name: `AM' MV` (IVE 'I AM'), `(See that?)" M/V` (NMIXX), `DDU-DU)' M,'V` (BLACKPINK),
/// `WHAT!' Special Video` (KISS OF LIFE). A closing quote glued to the end of a word and followed
/// straight by a K-pop format is specific enough on its own; "Official Music Video" does not
/// qualify here either, since Western labels use it too.
fn closed_quote_shape() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r#"(?i)[^\s'"‘’“”`´]['’"”•´]\s*(?:{MV_TOKEN}|official\s?mv|{LABEL_FORMATS}){R}"#
        ))
        .expect("closed quote regex")
    })
}

/// Any music-video word, quoted song or not. Only ever a co-signal for an ambiguous name.
fn mv_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?i){L}(?:{MV_TOKEN}|music\s?video|official\s?(?:video|audio)|performance|stage|choreography|dance\s?(?:practice|ver)|fancam|comeback|teaser|visualizer|lyrics|concert|special\s?(?:visual\s?)?(?:clip|video|film)|visual\s?clip|music\s?visual|album){R}"
        ))
        .expect("mv word regex")
    })
}

/// `MV` / `M/V` and the ways the band misreads it, or the words "music video" — the mark that
/// makes a Korean title a K-pop one.
fn mv_mark() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(?i){L}(?:{MV_TOKEN}|music\s?video){R}")).expect("mv mark regex"))
}

fn distinct_artists() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"(?i){L}(?:{}){R}", DISTINCT_ARTISTS.join("|"))).expect("artist regex")
    })
}

fn cased_artists() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let names: Vec<String> = CASED_ARTISTS.iter().map(|name| regex::escape(name)).collect();
        Regex::new(&format!(r"{L}(?:{}){R}", names.join("|"))).expect("cased regex")
    })
}

fn ive_series() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(IVE_SERIES).expect("ive series regex"))
}

fn ambiguous_artists() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let names: Vec<String> = AMBIGUOUS_ARTISTS.iter().map(|name| regex::escape(name)).collect();
        Regex::new(&format!(
            r"{L}(?:{}){R}|(?i:{L}(?:{}){R})",
            names.join("|"),
            AMBIGUOUS_PHRASES.join("|")
        ))
        .expect("ambiguous regex")
    })
}

/// A cased ambiguous name followed straight by a quoted title — the label convention
/// `ARTIST 'Song'`, which is what `IVE 'POST CREDIT SCENE : GIRL HERO Video` has and no
/// music-video word does. Western titles quote a song after a dash, if at all, so the
/// name-then-quote order is the signal. Not the any-case phrases: "Dreamcatcher 'Easy' Tutorial"
/// is a craft video.
fn ambiguous_then_quote() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let names: Vec<String> = AMBIGUOUS_ARTISTS.iter().map(|name| regex::escape(name)).collect();
        Regex::new(&format!(r#"{L}(?:{})\s*['‘"“•]"#, names.join("|"))).expect("ambiguous quote regex")
    })
}

fn format_markers() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"(?i){L}(?:{}){R}", FORMAT_MARKERS.join("|"))).expect("format regex")
    })
}

/// Hangul syllables or jamo anywhere in the text.
pub fn has_hangul(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c, '\u{AC00}'..='\u{D7A3}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}')
    })
}

fn has_hangul_marker(text: &str) -> bool {
    // The cheap test first: this runs over ~50,000 description openings on every save, and almost
    // none of them hold a Hangul character at all.
    has_hangul(text) && HANGUL_MARKERS.iter().any(|marker| text.contains(marker))
}

/// Matches video titles against the built-in rules plus a library's own additions
/// (`kpopExtraTerms` in the sidecar — a new debut group is a text edit, not a rebuild).
pub struct Matcher {
    extra: Vec<String>,
}

impl Matcher {
    pub fn new(extra_terms: &[String]) -> Self {
        Self {
            extra: extra_terms
                .iter()
                .map(|term| term.trim().to_lowercase())
                .filter(|term| !term.is_empty())
                .collect(),
        }
    }

    /// Whether any of the readings of one frame's title marks it as K-pop. The English and the
    /// Korean OCR of the same strip are separate readings, and a signal in either one counts — an
    /// ambiguous name read by one engine pairs with Hangul read by the other.
    pub fn matches_any(&self, readings: &[&str]) -> bool {
        let readings: Vec<&str> = readings.iter().copied().filter(|r| !r.trim().is_empty()).collect();
        if readings.is_empty() {
            return false;
        }
        if readings.iter().any(|reading| self.strong(reading)) {
            return true;
        }
        if readings.iter().any(|reading| ambiguous_then_quote().is_match(reading)) {
            return true;
        }
        // A Korean title marked as a music video: `바비 (BBIB) - BUMPA Official M/V`. Hangul alone is
        // not K-pop (Korean driving videos are in this library too); Hangul with `M/V` is.
        if readings.iter().any(|reading| has_hangul(reading)) && readings.iter().any(|reading| mv_mark().is_match(reading)) {
            return true;
        }
        let ambiguous = readings.iter().any(|reading| ambiguous_artists().is_match(reading));
        ambiguous && readings.iter().any(|reading| co_signal(reading))
    }

    /// An ambiguous name in `title` confirmed by a music-video word in `context` — the description's
    /// opening, for a title the vision model quoted: "FIFTY FIFTY - Starry Night" over "This is a
    /// video still from a YouTube music video".
    pub fn ambiguous_confirmed_by(&self, title: &str, context: &str) -> bool {
        ambiguous_artists().is_match(title) && co_signal(context)
    }

    fn strong(&self, title: &str) -> bool {
        format_markers().is_match(title)
            || mv_shape().is_match(title)
            || closed_quote_shape().is_match(title)
            || distinct_artists().is_match(title)
            || cased_artists().is_match(title)
            || ive_series().is_match(title)
            || has_hangul_marker(title)
            || self.extra_match(title)
    }

    /// Whether the opening of a vision description says the picture itself is K-pop: "This is an
    /// album cover photo for the K-pop group Crayon Pop", "a video still from the "Black Mamba"
    /// music video by aespa". Read from the description's first sentences only — further down, the
    /// model lists thumbnails on a YouTube page ("…a K-pop artist titled izna PLAYLIST…"), which
    /// says nothing about what the screenshot shows.
    ///
    /// The title rules apply to the prose as they do to a title — which is how "the music video
    /// featuring the artist BIBI" and "an album cover graphic for the group IVE" count: an
    /// ambiguous name next to a music word.
    pub fn describes_kpop(&self, lead: &str) -> bool {
        !lead.is_empty() && (kpop_word().is_match(lead) || self.matches_any(&[lead]))
    }

    fn extra_match(&self, title: &str) -> bool {
        if self.extra.is_empty() {
            return false;
        }
        let lower = title.to_lowercase();
        self.extra.iter().any(|term| contains_bounded(&lower, term))
    }
}

fn kpop_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(?i){L}(?:k[\s-]?pop|girl\s?group){R}")).expect("kpop word regex"))
}

/// Whether a description's opening says it shows a music video or a performance. The test a tab
/// strip has to pass: a title line listing several tabs ("FM - Crayon Pop - Deezer", "YouTube",
/// "The Consumerism Slop Epic…") names what is OPEN, not what is on screen — measured on this
/// library, 48 of the 52 Low Text frames with Crayon Pop in their tab strip were a motorcycle
/// video playing in another tab while the song played in Deezer.
pub fn describes_music_video(lead: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)music\s?video|music\s?visual|k[\s-]?pop|idol|girl\s?group|choreograph|dance\s(?:practice|routine|performance)|performing\s(?:a\s)?(?:dance|choreograph|on\s(?:a\s)?stage)|fancam|album\scover",
        )
        .expect("music video regex")
    })
    .is_match(lead)
}

/// What lets an ambiguous name count: a music-video word, Hangul (the Korean reading of the same
/// title), or anything that would have matched on its own.
fn co_signal(title: &str) -> bool {
    mv_word().is_match(title) || has_hangul(title) || format_markers().is_match(title)
}

/// `term` occurs in `haystack` with no ASCII letter or digit touching either end. A term that is
/// not plain ASCII (Hangul, mostly) is a plain substring — see [`L`] for why.
pub fn contains_bounded(haystack: &str, term: &str) -> bool {
    if !term.is_ascii() {
        return haystack.contains(term);
    }
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(term) {
        let start = from + offset;
        let end = start + term.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();
        if !is_word(before) && !is_word(after) {
            return true;
        }
        from = start + term.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// What one frame says about the session it was captured in, for [`fill_from_neighbours`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Its title marks it as K-pop.
    Kpop,
    /// It has a title, and it is something else — the session moved on.
    Other,
    /// Scanned, no title found (a tabbed browser, a fullscreen player), and Low Text: a frame that
    /// could be filled in from its neighbours.
    Candidate,
}

/// Untitled frames captured between two K-pop frames, each within `window` seconds, are K-pop too.
///
/// The frame that motivated it: a Vivaldi window with its tab strip showing, where the title band
/// reads tab names ("fromis_9 (프로미…") with no "- YouTube" marker and the frame is stored as
/// untitled. Both sides must agree and both must be close — a session that switched from an MV to a
/// driving video has an `Other` on one side, and the fill stops there.
///
/// Only titled frames count as neighbours; a run of untitled candidates in a row all look past each
/// other to the nearest real title on each side. Input order does not matter — including among
/// frames captured in the same second, which are ordered by key: the caller builds the list from a
/// HashMap, and ordering ties by arrival let two saves of the same library fill different frames.
pub fn fill_from_neighbours<K: Clone + Ord>(mut frames: Vec<(i64, Evidence, K)>, window: i64) -> Vec<K> {
    frames.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    let mut previous_titled: Vec<Option<usize>> = vec![None; frames.len()];
    let mut last = None;
    for (index, (_, evidence, _)) in frames.iter().enumerate() {
        previous_titled[index] = last;
        if *evidence != Evidence::Candidate {
            last = Some(index);
        }
    }
    let mut filled = Vec::new();
    let mut next = None;
    for index in (0..frames.len()).rev() {
        let (ts, evidence, key) = &frames[index];
        if *evidence == Evidence::Candidate {
            if let (Some(before), Some(after)) = (previous_titled[index], next) {
                let (before_ts, before_evidence, _) = &frames[before];
                let (after_ts, after_evidence, _): &(i64, Evidence, K) = &frames[after];
                if *before_evidence == Evidence::Kpop
                    && *after_evidence == Evidence::Kpop
                    && ts - before_ts <= window
                    && after_ts - ts <= window
                {
                    filled.push(key.clone());
                }
            }
        } else {
            next = Some(index);
        }
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kpop(title: &str) -> bool {
        Matcher::new(&[]).matches_any(&[title])
    }

    #[test]
    fn real_titles_from_the_library_match() {
        // Verbatim `video_title`s from the live library, OCR noise included.
        for title in [
            "9 (2) [MV] (G)I-DLE _ LATATA",
            "LE SSERAFIM x ILLIT x KATSEYE 'ICONIC BY MISTAKE Official MV",
            "(1) aespa 'LEMONADE MV",
            "Hearts2Hearts 'Lemon Tang• MV",
            "[Official MIV] cosmosy 'Chance — Al-B•LOleF _",
            "(2) [OffKial MIVI cosmosy 'Silence body & soul",
            "(2) STAYC(4E/OlAl) 'Bubble• MV",
            "9 (fromis_9) - 'Feel Good (SECRET CODE)' M,'V",
            "ITZY \"UNTOUCHABLE\" MIV @ITZY",
            "9 (2) IVE 'OffThe Record' MV",
            "'Girls Never Die• Official MV",
            "(1) AtHeart 'Say It Official MV",
            "GFRIEND 'MAGO' Official M,'V",
            "ARTMS - Blue Blood I Show! MusicCore I MBC2608082±",
            "9 [VKP] TOP 20 MAY Girl Groups I Played the Most [4K/MV]",
            "KPOP PLAYLIST 2026 Mid-Year Girl Group MVs [4K] Collection",
            "Kepler I 'BUBBLE GUW Performance Video",
        ] {
            assert!(kpop(title), "should match: {title:?}");
        }
    }

    #[test]
    fn titles_the_first_rules_missed_now_match() {
        // Verbatim Low Text titles from the live library (2026-10-20), each a miss of the 10-18
        // rules: frame counts in the comments.
        for title in [
            "tripleS(E21 moon 'Dream Dress' Special Visual Clip", // 20 — `Special Visual Clip`, cased name
            "DNA - tripleS but without the ring ring ring ring",  // fan edit, no format word at all
            "9 Kiss OF LIFE WHAT!' Special Video",                // casing + lost opening quote
            "e e e (See that?)\" M/V",                            // 22 — NMIXX, name lost
            "9 • (2) AM' MV",                                     // IVE 'I AM', name lost
            "9 (2) - (DDU-DU DDU-DU)' M,'V",                      // BLACKPINK, name lost
            "LOVE-lution capitav,sm• MV",                         // tripleS sub-unit
            "IVE 'POST CREDIT SCENE : GIRL HERO Video",           // 7 — name then quote
            "[IVE OFF] ag019h Dreams come true",                  // IVE's channel series
            "OI 6)ÖEÖ6) in WORLD TOUR I NYOUNG LOG [IVE LOGI 21",
            "uH%Ol -201 Q DÖEÖ6) in WORLD TOUR I NYOUNG LOG",
            "EVOLution 'Invincible' MV",
        ] {
            assert!(kpop(title), "should match: {title:?}");
        }
    }

    #[test]
    fn the_wider_rules_still_keep_lookalikes_out() {
        for title in [
            "How to sun.'ive re-entry",
            "IVe'",
            "NATURE LIVE 4K - Relaxing Forest Sounds",
            "Kiss of Life - CPR Training Video",
            "How to Make a Dreamcatcher 'Easy' Tutorial",
            "Evolution of the Mercedes S-Class",
            "The Biggest Helicopter Fly-ln of 2026! VAI Verticon Arrivals Day 1 & 2",
            "Driver's View: Lausanne Metro Line MI I Lausanne-Flon to Renens VD",
        ] {
            assert!(!kpop(title), "should not match: {title:?}");
        }
    }

    #[test]
    fn a_description_lead_counts_only_when_it_names_the_picture() {
        let matcher = Matcher::new(&[]);
        assert!(matcher.describes_kpop("This is an album cover photo for the K-pop group Crayon Pop."));
        assert!(matcher.describes_kpop("This is a screenshot from the \"Black Mamba\" music video by aespa."));
        assert!(!matcher.describes_kpop("This is a screenshot of a YouTube video featuring a three-panel collage."));
        assert!(describes_music_video("This is a video still from a music video featuring five women."));
        assert!(!describes_music_video("This is a screenshot of a YouTube video showing a motorcycle dashboard."));
    }

    #[test]
    fn known_false_positives_stay_out() {
        for title in [
            "The Village Where the Sun Rises Twice — The Most Beautiful in Transylvania",
            "LATATA - Scenic Drive in Poland PL I From Pu*tusk to Warsaw Through the Land of Lakes",
            "(1) It's Official, There Are Too Many Satellites!",
            "I saw the future in Korea",
            "Winter Drive in Lapland 4K",
            "Unbelievable Train Moments Caught on Camera",
            "Nirvana - \"Smells Like Teen Spirit\" (Official Music Video)",
            "HOW I'VE BEEN LIVING IN A VAN FOR 3 YEARS",
            "Red Velvet Cake Recipe",
        ] {
            assert!(!kpop(title), "should not match: {title:?}");
        }
    }

    #[test]
    fn hangul_reading_lets_an_ambiguous_name_count() {
        let matcher = Matcher::new(&[]);
        // English OCR of the IVE Everland video loses the Hangul; the Korean reading keeps it.
        assert!(!matcher.matches_any(&["IVE ForEVER I with EVERLAND"]));
        assert!(matcher.matches_any(&["IVE ForEVER I with EVERLAND", "IVE와 함께 ForEVER | with EVERLAND"]));
        // A Hangul-only playlist title names its genre in Hangul.
        assert!(matcher.matches_any(&["[Playlist] 요즘 이런 느낌의 여자아이돌 노래가 좋다"]));
        // Hangul alone is not K-pop: Korean driving videos are in this library too.
        assert!(!matcher.matches_any(&["서울 야간 드라이브 4K"]));
    }

    #[test]
    fn extra_terms_are_bounded_and_case_insensitive() {
        let matcher = Matcher::new(&["  NewGroup ".to_string(), "새그룹".to_string()]);
        assert!(matcher.matches_any(&["newgroup 'Debut' Teaser"]));
        assert!(matcher.matches_any(&["NEWGROUP live"]));
        assert!(!matcher.matches_any(&["renewgroupings explained"]));
        assert!(matcher.matches_any(&["새그룹 데뷔"]));
    }

    #[test]
    fn neighbours_fill_only_between_two_close_kpop_frames() {
        use Evidence::*;
        let frames = vec![
            (100, Kpop, "k1"),
            (103, Candidate, "a"),
            (110, Candidate, "b"),
            (130, Kpop, "k2"),
            (140, Candidate, "c"), // next titled frame is Other
            (150, Other, "o"),
            (160, Candidate, "d"), // previous titled is Other
            (170, Kpop, "k3"),
            (175, Candidate, "e"), // next Kpop is too far
            (900, Kpop, "k4"),
            (905, Candidate, "f"), // nothing after it
        ];
        let mut filled = fill_from_neighbours(frames, 120);
        filled.sort();
        assert_eq!(filled, vec!["a", "b"]);
    }

    #[test]
    fn neighbour_fill_ignores_input_order() {
        use Evidence::*;
        let frames = vec![(130, Kpop, 2), (103, Candidate, 1), (100, Kpop, 0)];
        assert_eq!(fill_from_neighbours(frames, 120), vec![1]);
    }

    #[test]
    fn frames_from_the_same_second_fill_the_same_way_in_any_order() {
        use Evidence::*;
        // A candidate sharing its second with a K-pop frame: after it, its previous title is K-pop
        // and it fills; before it, its previous title is the Other and it does not. Which one it
        // got used to be whatever order the HashMap produced.
        let frames = vec![(100, Other, "o"), (110, Kpop, "b-k"), (110, Candidate, "a-cand"), (120, Kpop, "k2")];
        let mut reversed = frames.clone();
        reversed.reverse();
        assert_eq!(fill_from_neighbours(frames, 120), fill_from_neighbours(reversed, 120));
    }
}
