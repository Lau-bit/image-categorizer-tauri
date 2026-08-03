//! Geographic tagging layer — a parallel axis over the vision descriptions.
//!
//! This never touches `.image-categorizer.json`. An image's `category` (Low Text / High Text / …)
//! stays exactly as it was; geo lives in its own sidecars so the whole layer can be deleted and
//! re-derived without risking the library's real classification. Three files, one purpose each:
//!
//! * `.image-categorizer-geo.json`       — the derived per-image records (regenerable, never edited)
//! * `.image-categorizer-geo-gazetteer.json` — the ONE hand-editable table that carries all the
//!   judgement: location-string overrides plus the fiction denylist. Fix a row here, re-derive, and
//!   every image that used that string is corrected.
//! * `.image-categorizer-geo-sets.json`  — built country sets (ordered member lists + provenance)
//!
//! The whole layer is a pure function of *(vision descriptions × gazetteer × chunk plan)*, so a
//! re-derive over ~10k descriptions is a couple of seconds and can just run after every scan.
//!
//! Two properties are worth knowing before changing anything here:
//!
//! **Location is a property of the video, not the frame.** The chunk plan samples ~10 frames per
//! video for the expensive vision pass, but a video shot in Japan is in Japan for all 368 of its
//! frames. Propagating a group's resolved countries to its unsampled members roughly doubles the
//! tagged population for free — it is where half the yield comes from.
//!
//! **Variety is measured in videos, not images.** A country with 400 images drawn from two videos
//! cannot produce a useful set: sixteen frames of two drives teaches one road, not a country, which
//! for geoguessing practice is worse than nothing because it trains a false prior. Every count that
//! decides anything here is therefore a count of distinct *sources* (chunk groups), and images-per-
//! country is reported alongside only as context.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const GEO_FILE_NAME: &str = ".image-categorizer-geo.json";
pub const GEO_GAZETTEER_FILE_NAME: &str = ".image-categorizer-geo-gazetteer.json";
pub const GEO_SETS_FILE_NAME: &str = ".image-categorizer-geo-sets.json";
/// Images kicked out of set building by hand — the escape hatch for pictures that carry a real
/// country but are useless as geography (a portrait, a UI screenshot, a rollercoaster). Written by
/// whichever app spots them (super-image-viewer has a context-menu action) and honoured here, so an
/// exclusion survives every rebuild instead of being undone by the next one.
pub const GEO_EXCLUDED_FILE_NAME: &str = ".image-categorizer-geo-excluded.json";

pub const GEO_SCHEMA_VERSION: u32 = 1;

/// Distinct video sources a country needs before a set built from it is genuinely varied. Sets are
/// still built below this — the user explicitly wants thin countries visible and openable — but they
/// are badged `limited` so a two-video "Canada" can never be mistaken for real practice material.
pub const DIVERSITY_FLOOR: usize = 16;
/// Sources above which a country has enough material for several non-overlapping sets.
pub const DEEP_FLOOR: usize = 32;
/// Below this a country is a token: you have seen the place, you cannot practise it.
pub const SEED_CEILING: usize = 3;

pub const DEFAULT_SET_SIZE: usize = 16;

/// How many unresolved strings the coverage view surfaces. The full list stays in the gazetteer
/// file; this is just the top of it, and a few dozen is already more than one sitting's work.
const WORKLIST_LIMIT: usize = 40;

// ---------------------------------------------------------------------------------------------
// Country reference
// ---------------------------------------------------------------------------------------------

// The reference spine. Coverage is a LEFT JOIN of this list against the derived records — without
// it a country with zero images simply would not exist in the output, and "which countries am I
// missing" is the whole point of the coverage view.
//
// Deliberately ~109 entries, not all 195 UN states: padding the board with Nauru and Tuvalu makes it
// permanently red and trains you to ignore it. These are the countries that realistically turn up in
// street-level / drone / driving footage.
//
// Clusters are confusion neighbourhoods, not continents. Grouping this way is what makes the view
// actionable for km-distance play: "Baltics 0/5" tells you where distance points are being lost in a
// way that an alphabetical list of 109 rows never will.
const COUNTRIES: &[(&str, &str, &str)] = &[
    // (name, iso2, cluster)
    ("Finland", "FI", "Nordics"),
    ("Sweden", "SE", "Nordics"),
    ("Norway", "NO", "Nordics"),
    ("Denmark", "DK", "Nordics"),
    ("Iceland", "IS", "Nordics"),
    ("Estonia", "EE", "Baltics & Poland"),
    ("Latvia", "LV", "Baltics & Poland"),
    ("Lithuania", "LT", "Baltics & Poland"),
    ("Poland", "PL", "Baltics & Poland"),
    ("Belarus", "BY", "Baltics & Poland"),
    ("Croatia", "HR", "Balkans"),
    ("Serbia", "RS", "Balkans"),
    ("Bosnia and Herzegovina", "BA", "Balkans"),
    ("Montenegro", "ME", "Balkans"),
    ("North Macedonia", "MK", "Balkans"),
    ("Albania", "AL", "Balkans"),
    ("Slovenia", "SI", "Balkans"),
    ("Bulgaria", "BG", "Balkans"),
    ("Romania", "RO", "Balkans"),
    ("Greece", "GR", "Balkans"),
    ("France", "FR", "Western Europe"),
    ("Germany", "DE", "Western Europe"),
    ("Netherlands", "NL", "Western Europe"),
    ("Belgium", "BE", "Western Europe"),
    ("Austria", "AT", "Western Europe"),
    ("Switzerland", "CH", "Western Europe"),
    ("Ireland", "IE", "Western Europe"),
    ("United Kingdom", "GB", "Western Europe"),
    ("Luxembourg", "LU", "Western Europe"),
    ("Spain", "ES", "Iberia & Italy"),
    ("Portugal", "PT", "Iberia & Italy"),
    ("Italy", "IT", "Iberia & Italy"),
    ("Andorra", "AD", "Iberia & Italy"),
    ("Malta", "MT", "Iberia & Italy"),
    ("Ukraine", "UA", "Eastern Europe"),
    ("Russia", "RU", "Eastern Europe"),
    ("Czechia", "CZ", "Eastern Europe"),
    ("Georgia", "GE", "Eastern Europe"),
    ("Slovakia", "SK", "Eastern Europe"),
    ("Hungary", "HU", "Eastern Europe"),
    ("Moldova", "MD", "Eastern Europe"),
    ("Thailand", "TH", "Southeast Asia"),
    ("Malaysia", "MY", "Southeast Asia"),
    ("Indonesia", "ID", "Southeast Asia"),
    ("Philippines", "PH", "Southeast Asia"),
    ("Vietnam", "VN", "Southeast Asia"),
    ("Cambodia", "KH", "Southeast Asia"),
    ("Laos", "LA", "Southeast Asia"),
    ("Myanmar", "MM", "Southeast Asia"),
    ("Singapore", "SG", "Southeast Asia"),
    ("Japan", "JP", "East Asia"),
    ("South Korea", "KR", "East Asia"),
    ("China", "CN", "East Asia"),
    ("Taiwan", "TW", "East Asia"),
    ("Hong Kong", "HK", "East Asia"),
    ("Mongolia", "MN", "East Asia"),
    ("India", "IN", "South Asia"),
    ("Pakistan", "PK", "South Asia"),
    ("Bangladesh", "BD", "South Asia"),
    ("Sri Lanka", "LK", "South Asia"),
    ("Nepal", "NP", "South Asia"),
    ("Bhutan", "BT", "South Asia"),
    ("Brazil", "BR", "Latin America"),
    ("Argentina", "AR", "Latin America"),
    ("Chile", "CL", "Latin America"),
    ("Peru", "PE", "Latin America"),
    ("Colombia", "CO", "Latin America"),
    ("Bolivia", "BO", "Latin America"),
    ("Ecuador", "EC", "Latin America"),
    ("Uruguay", "UY", "Latin America"),
    ("Paraguay", "PY", "Latin America"),
    ("Mexico", "MX", "Latin America"),
    ("Guatemala", "GT", "Latin America"),
    ("Costa Rica", "CR", "Latin America"),
    ("Panama", "PA", "Latin America"),
    ("South Africa", "ZA", "Africa"),
    ("Kenya", "KE", "Africa"),
    ("Nigeria", "NG", "Africa"),
    ("Ghana", "GH", "Africa"),
    ("Tanzania", "TZ", "Africa"),
    ("Uganda", "UG", "Africa"),
    ("Senegal", "SN", "Africa"),
    ("DR Congo", "CD", "Africa"),
    ("Liberia", "LR", "Africa"),
    ("Ethiopia", "ET", "Africa"),
    ("Botswana", "BW", "Africa"),
    ("Namibia", "NA", "Africa"),
    ("Rwanda", "RW", "Africa"),
    ("Zambia", "ZM", "Africa"),
    ("Zimbabwe", "ZW", "Africa"),
    ("Mozambique", "MZ", "Africa"),
    ("Turkey", "TR", "Middle East & North Africa"),
    ("Egypt", "EG", "Middle East & North Africa"),
    ("Morocco", "MA", "Middle East & North Africa"),
    ("Tunisia", "TN", "Middle East & North Africa"),
    ("Israel", "IL", "Middle East & North Africa"),
    ("Jordan", "JO", "Middle East & North Africa"),
    ("UAE", "AE", "Middle East & North Africa"),
    ("Saudi Arabia", "SA", "Middle East & North Africa"),
    ("Oman", "OM", "Middle East & North Africa"),
    ("Qatar", "QA", "Middle East & North Africa"),
    ("Kuwait", "KW", "Middle East & North Africa"),
    ("Bahrain", "BH", "Middle East & North Africa"),
    ("Iran", "IR", "Middle East & North Africa"),
    ("Iraq", "IQ", "Middle East & North Africa"),
    ("Lebanon", "LB", "Middle East & North Africa"),
    ("Australia", "AU", "Oceania & North America"),
    ("New Zealand", "NZ", "Oceania & North America"),
    ("United States", "US", "Oceania & North America"),
    ("Canada", "CA", "Oceania & North America"),
];

// Cluster display order — the coverage view reads top-down, so the clusters the user actually plays
// most come first rather than falling out in country order.
const CLUSTER_ORDER: &[&str] = &[
    "Nordics",
    "Baltics & Poland",
    "Balkans",
    "Western Europe",
    "Iberia & Italy",
    "Eastern Europe",
    "Southeast Asia",
    "East Asia",
    "South Asia",
    "Latin America",
    "Africa",
    "Middle East & North Africa",
    "Oceania & North America",
];

/// Aliases resolved with full confidence: country names, unambiguous demonyms, and cities/regions
/// distinctive enough that they identify exactly one country. Extend freely — this is the cheap way
/// to raise coverage, and every entry added here fixes every image that mentions it.
const STRONG_ALIASES: &[(&str, &str)] = &[
    // Multi-word country forms and abbreviations
    ("usa", "United States"),
    ("u s a", "United States"),
    ("united states of america", "United States"),
    ("america", "United States"),
    ("uk", "United Kingdom"),
    ("great britain", "United Kingdom"),
    ("britain", "United Kingdom"),
    ("england", "United Kingdom"),
    ("scotland", "United Kingdom"),
    ("wales", "United Kingdom"),
    ("northern ireland", "United Kingdom"),
    ("korea", "South Korea"),
    ("republic of korea", "South Korea"),
    ("drc", "DR Congo"),
    ("dr congo", "DR Congo"),
    ("democratic republic of the congo", "DR Congo"),
    ("democratic republic of congo", "DR Congo"),
    ("congo", "DR Congo"),
    ("uae", "UAE"),
    ("united arab emirates", "UAE"),
    ("czech republic", "Czechia"),
    ("turkiye", "Turkey"),
    ("holland", "Netherlands"),
    ("the netherlands", "Netherlands"),
    ("bosnia", "Bosnia and Herzegovina"),
    ("herzegovina", "Bosnia and Herzegovina"),
    ("macedonia", "North Macedonia"),
    ("burma", "Myanmar"),
    // Nordics
    ("helsinki", "Finland"),
    ("tampere", "Finland"),
    ("turku", "Finland"),
    ("lapland", "Finland"),
    ("stockholm", "Sweden"),
    ("gothenburg", "Sweden"),
    ("malmo", "Sweden"),
    ("oslo", "Norway"),
    ("bergen", "Norway"),
    ("lofoten", "Norway"),
    ("tromso", "Norway"),
    ("copenhagen", "Denmark"),
    ("aarhus", "Denmark"),
    ("reykjavik", "Iceland"),
    // Baltics & Poland
    ("tallinn", "Estonia"),
    ("riga", "Latvia"),
    ("vilnius", "Lithuania"),
    ("warsaw", "Poland"),
    ("warszawa", "Poland"),
    ("krakow", "Poland"),
    ("gdansk", "Poland"),
    ("wroclaw", "Poland"),
    ("minsk", "Belarus"),
    // Balkans
    ("zagreb", "Croatia"),
    ("dubrovnik", "Croatia"),
    ("split", "Croatia"),
    ("belgrade", "Serbia"),
    ("sarajevo", "Bosnia and Herzegovina"),
    ("podgorica", "Montenegro"),
    ("kotor", "Montenegro"),
    ("skopje", "North Macedonia"),
    ("ohrid", "North Macedonia"),
    ("tirana", "Albania"),
    ("ljubljana", "Slovenia"),
    ("sofia", "Bulgaria"),
    ("bucharest", "Romania"),
    ("transylvania", "Romania"),
    ("athens", "Greece"),
    ("santorini", "Greece"),
    ("crete", "Greece"),
    ("thessaloniki", "Greece"),
    // Western Europe
    ("paris", "France"),
    ("marseille", "France"),
    ("lyon", "France"),
    ("nice", "France"),
    ("bordeaux", "France"),
    ("normandy", "France"),
    ("brittany", "France"),
    ("corsica", "France"),
    ("berlin", "Germany"),
    ("munich", "Germany"),
    ("hamburg", "Germany"),
    ("cologne", "Germany"),
    ("frankfurt", "Germany"),
    ("bavaria", "Germany"),
    ("black forest", "Germany"),
    ("amsterdam", "Netherlands"),
    ("rotterdam", "Netherlands"),
    ("utrecht", "Netherlands"),
    ("the hague", "Netherlands"),
    ("brussels", "Belgium"),
    ("antwerp", "Belgium"),
    ("bruges", "Belgium"),
    ("ghent", "Belgium"),
    ("vienna", "Austria"),
    ("salzburg", "Austria"),
    ("innsbruck", "Austria"),
    ("tyrol", "Austria"),
    ("zurich", "Switzerland"),
    ("geneva", "Switzerland"),
    ("bern", "Switzerland"),
    ("lucerne", "Switzerland"),
    ("interlaken", "Switzerland"),
    ("zermatt", "Switzerland"),
    ("dublin", "Ireland"),
    ("cork", "Ireland"),
    ("london", "United Kingdom"),
    ("manchester", "United Kingdom"),
    ("liverpool", "United Kingdom"),
    ("birmingham", "United Kingdom"),
    ("glasgow", "United Kingdom"),
    ("edinburgh", "United Kingdom"),
    ("cardiff", "United Kingdom"),
    ("belfast", "United Kingdom"),
    ("yorkshire", "United Kingdom"),
    ("yorkshire dales", "United Kingdom"),
    ("cotswolds", "United Kingdom"),
    ("lake district", "United Kingdom"),
    ("cornwall", "United Kingdom"),
    ("snowdonia", "United Kingdom"),
    ("gordale scar", "United Kingdom"),
    // Iberia & Italy
    ("madrid", "Spain"),
    ("barcelona", "Spain"),
    ("valencia", "Spain"),
    ("seville", "Spain"),
    ("andalusia", "Spain"),
    ("mallorca", "Spain"),
    ("canary islands", "Spain"),
    ("tenerife", "Spain"),
    ("lisbon", "Portugal"),
    ("porto", "Portugal"),
    ("madeira", "Portugal"),
    ("algarve", "Portugal"),
    ("rome", "Italy"),
    ("milan", "Italy"),
    ("naples", "Italy"),
    ("venice", "Italy"),
    ("florence", "Italy"),
    ("turin", "Italy"),
    ("sicily", "Italy"),
    ("sardinia", "Italy"),
    ("tuscany", "Italy"),
    ("dolomites", "Italy"),
    ("amalfi", "Italy"),
    // Eastern Europe
    ("kyiv", "Ukraine"),
    ("kiev", "Ukraine"),
    ("lviv", "Ukraine"),
    ("odesa", "Ukraine"),
    ("odessa", "Ukraine"),
    ("kharkiv", "Ukraine"),
    ("moscow", "Russia"),
    ("saint petersburg", "Russia"),
    ("st petersburg", "Russia"),
    ("siberia", "Russia"),
    ("vladivostok", "Russia"),
    ("kamchatka", "Russia"),
    ("prague", "Czechia"),
    ("brno", "Czechia"),
    ("bratislava", "Slovakia"),
    ("budapest", "Hungary"),
    ("chisinau", "Moldova"),
    // Southeast Asia
    ("bangkok", "Thailand"),
    ("phuket", "Thailand"),
    ("chiang mai", "Thailand"),
    ("pattaya", "Thailand"),
    ("lumpini park", "Thailand"),
    ("kuala lumpur", "Malaysia"),
    ("penang", "Malaysia"),
    ("borneo", "Malaysia"),
    ("jakarta", "Indonesia"),
    ("bali", "Indonesia"),
    ("sumatra", "Indonesia"),
    ("java", "Indonesia"),
    ("manila", "Philippines"),
    ("cebu", "Philippines"),
    ("hanoi", "Vietnam"),
    ("ho chi minh", "Vietnam"),
    ("saigon", "Vietnam"),
    ("phnom penh", "Cambodia"),
    ("angkor", "Cambodia"),
    ("vientiane", "Laos"),
    ("yangon", "Myanmar"),
    ("mandalay", "Myanmar"),
    ("bagan", "Myanmar"),
    // East Asia
    ("tokyo", "Japan"),
    ("osaka", "Japan"),
    ("kyoto", "Japan"),
    ("nagoya", "Japan"),
    ("sapporo", "Japan"),
    ("fukuoka", "Japan"),
    ("hokkaido", "Japan"),
    ("okinawa", "Japan"),
    ("honshu", "Japan"),
    ("aichi", "Japan"),
    ("shizuoka", "Japan"),
    ("hiroshima", "Japan"),
    ("yokohama", "Japan"),
    ("mount fuji", "Japan"),
    ("shibuya", "Japan"),
    ("shinjuku", "Japan"),
    ("onsen", "Japan"),
    ("seoul", "South Korea"),
    ("busan", "South Korea"),
    ("jeju", "South Korea"),
    ("incheon", "South Korea"),
    ("gangnam", "South Korea"),
    ("beijing", "China"),
    ("shanghai", "China"),
    ("shenzhen", "China"),
    ("guangzhou", "China"),
    ("chengdu", "China"),
    ("chongqing", "China"),
    ("xian", "China"),
    ("tibet", "China"),
    ("yunnan", "China"),
    ("taipei", "Taiwan"),
    ("ulaanbaatar", "Mongolia"),
    // South Asia
    ("mumbai", "India"),
    ("delhi", "India"),
    ("bangalore", "India"),
    ("bengaluru", "India"),
    ("kolkata", "India"),
    ("chennai", "India"),
    ("hyderabad", "India"),
    ("jaipur", "India"),
    ("kerala", "India"),
    ("goa", "India"),
    ("himalayas", "Nepal"),
    ("karachi", "Pakistan"),
    ("lahore", "Pakistan"),
    ("islamabad", "Pakistan"),
    ("dhaka", "Bangladesh"),
    ("colombo", "Sri Lanka"),
    ("kathmandu", "Nepal"),
    ("thimphu", "Bhutan"),
    // Latin America
    ("sao paulo", "Brazil"),
    ("rio de janeiro", "Brazil"),
    ("brasilia", "Brazil"),
    ("belo horizonte", "Brazil"),
    ("salvador", "Brazil"),
    ("curitiba", "Brazil"),
    ("amazon", "Brazil"),
    ("minas gerais", "Brazil"),
    ("vitoria minas railway", "Brazil"),
    ("buenos aires", "Argentina"),
    ("patagonia", "Argentina"),
    ("cordoba", "Argentina"),
    ("santiago", "Chile"),
    ("atacama", "Chile"),
    ("valparaiso", "Chile"),
    ("lima", "Peru"),
    ("cusco", "Peru"),
    ("machu picchu", "Peru"),
    ("bogota", "Colombia"),
    ("medellin", "Colombia"),
    ("cartagena", "Colombia"),
    ("la paz", "Bolivia"),
    ("quito", "Ecuador"),
    ("galapagos", "Ecuador"),
    ("montevideo", "Uruguay"),
    ("asuncion", "Paraguay"),
    ("mexico city", "Mexico"),
    ("cancun", "Mexico"),
    ("guadalajara", "Mexico"),
    ("yucatan", "Mexico"),
    ("guatemala city", "Guatemala"),
    ("san jose costa rica", "Costa Rica"),
    ("panama city", "Panama"),
    // Africa
    ("cape town", "South Africa"),
    ("johannesburg", "South Africa"),
    ("durban", "South Africa"),
    ("nairobi", "Kenya"),
    ("mombasa", "Kenya"),
    ("lagos", "Nigeria"),
    ("abuja", "Nigeria"),
    ("accra", "Ghana"),
    ("dar es salaam", "Tanzania"),
    ("zanzibar", "Tanzania"),
    ("kilimanjaro", "Tanzania"),
    ("kampala", "Uganda"),
    ("dakar", "Senegal"),
    ("kinshasa", "DR Congo"),
    ("matadi", "DR Congo"),
    ("monrovia", "Liberia"),
    ("addis ababa", "Ethiopia"),
    ("gaborone", "Botswana"),
    ("windhoek", "Namibia"),
    ("kigali", "Rwanda"),
    ("lusaka", "Zambia"),
    ("harare", "Zimbabwe"),
    ("maputo", "Mozambique"),
    // Middle East & North Africa
    ("istanbul", "Turkey"),
    ("ankara", "Turkey"),
    ("izmir", "Turkey"),
    ("antalya", "Turkey"),
    ("cappadocia", "Turkey"),
    ("bosphorus", "Turkey"),
    ("cairo", "Egypt"),
    ("alexandria", "Egypt"),
    ("giza", "Egypt"),
    ("luxor", "Egypt"),
    ("marrakech", "Morocco"),
    ("casablanca", "Morocco"),
    ("fez", "Morocco"),
    ("tangier", "Morocco"),
    ("tunis", "Tunisia"),
    ("jerusalem", "Israel"),
    ("tel aviv", "Israel"),
    ("amman", "Jordan"),
    ("petra", "Jordan"),
    ("dubai", "UAE"),
    ("abu dhabi", "UAE"),
    ("sharjah", "UAE"),
    ("riyadh", "Saudi Arabia"),
    ("jeddah", "Saudi Arabia"),
    ("mecca", "Saudi Arabia"),
    ("muscat", "Oman"),
    ("salalah", "Oman"),
    ("doha", "Qatar"),
    ("kuwait city", "Kuwait"),
    ("manama", "Bahrain"),
    ("tehran", "Iran"),
    ("baghdad", "Iraq"),
    ("beirut", "Lebanon"),
    // Oceania & North America
    ("sydney", "Australia"),
    ("melbourne", "Australia"),
    ("brisbane", "Australia"),
    ("perth", "Australia"),
    ("adelaide", "Australia"),
    ("tasmania", "Australia"),
    ("queensland", "Australia"),
    ("outback", "Australia"),
    ("auckland", "New Zealand"),
    ("wellington", "New Zealand"),
    ("queenstown", "New Zealand"),
    ("toronto", "Canada"),
    ("vancouver", "Canada"),
    ("montreal", "Canada"),
    ("calgary", "Canada"),
    ("ottawa", "Canada"),
    ("quebec", "Canada"),
    ("british columbia", "Canada"),
    ("alberta", "Canada"),
    ("ontario", "Canada"),
    ("banff", "Canada"),
    ("yukon", "Canada"),
];

/// US states, territories and major cities, all folding to one country. Kept as its own table purely
/// so the strong-alias list above stays readable — behaviourally identical.
const US_ALIASES: &[&str] = &[
    "alabama", "alaska", "arizona", "arkansas", "california", "colorado", "connecticut", "delaware",
    "florida", "hawaii", "idaho", "illinois", "indiana", "iowa", "kansas", "kentucky", "louisiana",
    "maine", "maryland", "massachusetts", "michigan", "minnesota", "mississippi", "missouri",
    "montana", "nebraska", "nevada", "new hampshire", "new jersey", "new mexico", "north carolina",
    "north dakota", "ohio", "oklahoma", "oregon", "pennsylvania", "rhode island", "south carolina",
    "south dakota", "tennessee", "texas", "utah", "vermont", "virginia", "washington",
    "west virginia", "wisconsin", "wyoming", "puerto rico",
    "new york", "new york city", "nyc", "manhattan", "brooklyn", "los angeles", "san francisco",
    "chicago", "houston", "phoenix", "philadelphia", "san diego", "dallas", "austin", "seattle",
    "denver", "boston", "atlanta", "miami", "las vegas", "portland", "detroit", "minneapolis",
    "new orleans", "nashville", "salt lake city", "san antonio", "sacramento", "kansas city",
    "times square", "manhattan skyline", "sierra nevada mountains", "grand canyon", "yosemite",
    "yellowstone", "appalachian", "rocky mountains", "skagway", "anchorage", "honolulu",
    "silicon valley", "hollywood", "midwest", "pacific northwest",
];

/// Tokens that name a real country but are common enough as a personal name, a US state, or an
/// ordinary English word that matching them alone produces nonsense. They resolve ONLY when nothing
/// stronger matched — so "Atlanta, Georgia" is the United States, while a bare "Georgia" is the
/// country. Without this split, every US southern-state caption silently became Caucasus footage.
const WEAK_ALIASES: &[(&str, &str)] = &[
    ("georgia", "Georgia"),
    ("jordan", "Jordan"),
    ("chad", "Chad"),
    ("mali", "Mali"),
    ("niger", "Niger"),
    ("guinea", "Guinea"),
    ("turkey", "Turkey"),
    ("china", "China"),
    ("india", "India"),
];

/// Location lines the vision model produced for pictures that have no geography at all. The model
/// was asked to state a location and obliged even for a close-up of a wall, so these are its polite
/// non-answers. They must be rejected rather than left unresolved, otherwise they clog the gazetteer
/// worklist forever with strings no human can ever map to a country.
/// Generic nouns that are a non-answer ONLY when they are the entire location line. They must never
/// prefix-match: "Port of Rotterdam, Netherlands" and "Road to Hana, Hawaii" both begin with one of
/// these and are perfectly good locations. Compared after a leading article is stripped, since the
/// model is inconsistent about articles.
const JUNK_EXACT: &[&str] = &[
    "forest",
    "forest clearing",
    "port",
    "harbor",
    "harbour",
    "city",
    "town",
    "village",
    "road",
    "highway",
    "street",
    "railway",
    "train station",
    "airport",
    "beach",
    "mountain",
    "mountains",
    "quarry",
    "marble quarry",
    "interchange",
    "warehouse",
    "factory",
    "military truck factory",
    "workshop",
    "garage",
    "kitchen",
    "bedroom",
    "living room",
    "office",
    "studio",
    "construction site",
    "mining site",
    "old mining site",
    "open pit mine",
    "river bend",
    "desert landscape",
    "urban city",
    "snowy mountains",
    "indoor",
    "indoors",
];

/// Ports of registry painted on a ship's stern. The vision model reads that text and dutifully
/// answers `Location: Panama` for a container ship filmed on the Elbe or the Congo, which is how a
/// library of European shipspotting footage grew a "Liberia" and a "Panama".
///
/// These reject ONLY when the whole location line is the bare port/flag AND the description is
/// about a vessel — "Panama City", "Colón, Panama" and "Panama Canal" all normalize differently and
/// survive, and a bare "Panama" in a description with no ship in it is left alone. Narrow on
/// purpose: this is a false-positive filter, not a country ban.
const REGISTRY_PORTS: &[&str] = &[
    "panama",
    "liberia",
    "monrovia",
    "majuro",
    "valletta",
    "limassol",
    "nassau",
    "road town",
    "port vila",
    "willemstad",
    "bridgetown",
    "marshall islands",
];

/// Words that make a description maritime enough for the registry-port rule to fire.
const VESSEL_WORDS: &[&str] = &[
    "ship",
    "ships",
    "vessel",
    "vessels",
    "tanker",
    "freighter",
    "barge",
    "tugboat",
    "shipspotting",
    "stern",
    "hull",
    "maersk",
    "cargo",
    "port of registry",
    "home port",
];

/// Phrases that are a non-answer however they continue — the model declining to name a place. Safe
/// to prefix-match because no real location line starts with one.
const JUNK_PREFIXES: &[&str] = &[
    "not specified",
    "not stated",
    "not applicable",
    "not identifiable",
    "not determinable",
    "n a",
    "unknown",
    "unspecified",
    "no location",
    "no specific location",
    "none",
];

/// Default group-title patterns marking a whole video as fictional. This lives at GROUP level on
/// purpose: a game screenshot's *description* reads exactly like a real place ("Exploring Empty BF4
/// Maps" resolved to China), so a per-image content filter catches almost none of it — a regex over
/// descriptions found under 4%. The video title is the only reliable tell, and there are ~1.5k of
/// those versus ~10k descriptions. Extend via the gazetteer file, not here.
const DEFAULT_FICTION_TITLE_PATTERNS: &[&str] = &[
    "exploring empty bf",
    "exploring bf",
    "exploring empty halo",
    "exploring halo",
    "halo 2",
    "halo 3",
    "halo infinite",
    "beamng",
    "gta v",
    "gta 5",
    "cyberpunk 2077",
    "microsoft flight simulator",
    "euro truck simulator",
    "american truck simulator",
    "assetto corsa",
    "forza horizon",
    "minecraft",
    "snowrunner",
    "farming simulator",
    "arma 3",
    "battlefield 1",
    "battlefield 3",
    "battlefield 4",
    "battlefield v",
];

// ---------------------------------------------------------------------------------------------
// Gazetteer — the one hand-editable table
// ---------------------------------------------------------------------------------------------

/// The editable decision table. `overrides` wins over every built-in rule, so any single bad call
/// can be corrected by hand without touching code; `unresolved` is regenerated on every derive and
/// is deliberately a *worklist* — location strings the resolver could not place, sorted by how many
/// images each would fix. Moving a line from `unresolved` into `overrides` and re-deriving is the
/// intended loop for raising coverage.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gazetteer {
    #[serde(default = "gazetteer_version")]
    pub version: u32,
    #[serde(default)]
    pub note: String,
    /// Raw location string (lowercased) -> country, `"A, B"` for a route, or `null` to reject it as
    /// non-geographic. An empty string rejects too.
    #[serde(default)]
    pub overrides: BTreeMap<String, Option<String>>,
    /// Extra group-title substrings that mark a video as fictional, on top of the built-ins.
    #[serde(default)]
    pub fiction_title_patterns: Vec<String>,
    /// Regenerated each derive: unplaceable strings and how many images each accounts for.
    #[serde(default)]
    pub unresolved: BTreeMap<String, u32>,
}

fn gazetteer_version() -> u32 {
    1
}

impl Default for Gazetteer {
    fn default() -> Self {
        Self {
            version: gazetteer_version(),
            note: GAZETTEER_NOTE.to_string(),
            overrides: BTreeMap::new(),
            fiction_title_patterns: Vec::new(),
            unresolved: BTreeMap::new(),
        }
    }
}

const GAZETTEER_NOTE: &str = "Hand-edit 'overrides' to fix geo tagging: \"location string\": \"Country\" \
to place it, \"Country A, Country B\" for a route that crosses a border, or null to reject it as \
non-geographic. Keys are lowercase. 'unresolved' is regenerated on every derive and lists what the \
resolver could not place, with the number of images each string would fix - work down it from the \
top. 'fictionTitlePatterns' adds video titles (substring match, lowercase) whose frames should be \
excluded as game or fictional footage.";

pub fn gazetteer_path(root: &Path) -> PathBuf {
    root.join(GEO_GAZETTEER_FILE_NAME)
}

pub fn load_gazetteer(root: &Path) -> Gazetteer {
    std::fs::read_to_string(gazetteer_path(root))
        .ok()
        .and_then(|text| serde_json::from_str::<Gazetteer>(&text).ok())
        .unwrap_or_default()
}

pub fn save_gazetteer(root: &Path, gazetteer: &Gazetteer) -> Result<(), String> {
    let json = serde_json::to_string_pretty(gazetteer)
        .map_err(|error| format!("Failed to serialize gazetteer: {error}"))?;
    std::fs::write(gazetteer_path(root), json)
        .map_err(|error| format!("Failed to save gazetteer: {error}"))
}

/// The hand-decision table on its own, for the worklist UI to show what has already been decided.
/// Read straight off disk rather than cached, so decisions made by hand-editing the file show up
/// in the app too — the file stays the single store, exactly as the review panel treats it.
pub fn overrides(root: &Path) -> BTreeMap<String, Option<String>> {
    load_gazetteer(root).overrides
}

/// Record one decision about one location string — the same edit `overrides` takes by hand, made
/// from the worklist instead. `action` is `place` (with `country`, or `"A, B"` for a route),
/// `reject` (non-geographic), or `clear` (hand it back to the resolver's own judgement).
///
/// Keys are stored lowercased, matching the hand-edit contract in `GAZETTEER_NOTE`; the resolver
/// runs `normalize()` over both sides at lookup time, so that is all the canonicalization needed.
/// Returns the whole table so a caller can repaint every row from one round trip.
pub fn set_override(
    root: &Path,
    location: &str,
    action: &str,
    country: Option<&str>,
) -> Result<BTreeMap<String, Option<String>>, String> {
    let key = location.trim().to_lowercase();
    if key.is_empty() {
        return Err("No location string to decide about.".to_string());
    }

    let mut gazetteer = load_gazetteer(root);
    match action {
        "place" => {
            // Stored in the same shape a hand-editor would write, so re-reading the file and
            // re-saving it from the UI can never rewrite someone's routes into another format.
            let parts: Vec<String> = country
                .unwrap_or("")
                .split(',')
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect();
            if parts.is_empty() {
                return Err("Type a country before saving.".to_string());
            }
            gazetteer.overrides.insert(key, Some(parts.join(", ")));
        }
        "reject" => {
            gazetteer.overrides.insert(key, None);
        }
        "clear" => {
            gazetteer.overrides.remove(&key);
        }
        other => return Err(format!("Unknown override action: {other}")),
    }

    if gazetteer.note.is_empty() {
        gazetteer.note = GAZETTEER_NOTE.to_string();
    }
    save_gazetteer(root, &gazetteer)?;
    Ok(gazetteer.overrides)
}

/// A cheap stable fingerprint of the hand-edited parts of the gazetteer. Stored on every derived
/// record and every built set, so a set can be spotted as predating a correction rather than
/// silently carrying stale tags.
fn gazetteer_fingerprint(gazetteer: &Gazetteer) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    for (key, value) in &gazetteer.overrides {
        key.hash(&mut hasher);
        value.hash(&mut hasher);
    }
    for pattern in &gazetteer.fiction_title_patterns {
        pattern.hash(&mut hasher);
    }
    hasher.finish()
}

// ---------------------------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// One or more countries. More than one means the footage crosses a border — a route video —
    /// which is a normal outcome, not an error: the image simply belongs to both country sets.
    Countries(Vec<String>),
    /// A location line that names no geography ("a forest", "not specified").
    Junk,
    /// Real-looking but unplaceable. These become the gazetteer worklist.
    Unresolved,
}

/// Lowercases and strips punctuation to a comparable form, keeping commas as part separators.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = true;
    for ch in text.chars() {
        let mapped = if ch.is_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if ch == ',' {
            ','
        } else {
            ' '
        };
        if mapped == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(mapped);
    }
    out.trim().trim_end_matches(',').trim().to_string()
}

/// Word-boundary containment. Plain `contains` would match "chad" inside "chadwick" and "mali"
/// inside "malignant", both of which turn up in description prose often enough to matter.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut start = 0;
    while start + n.len() <= h.len() {
        match haystack[start..].find(needle) {
            None => return false,
            Some(offset) => {
                let at = start + offset;
                let before_ok = at == 0 || !h[at - 1].is_ascii_alphanumeric();
                let after = at + n.len();
                let after_ok = after >= h.len() || !h[after].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    return true;
                }
                start = at + 1;
            }
        }
    }
    false
}

/// The built-in lookup tables, materialized once per derive rather than per image.
pub struct Resolver {
    strong: HashMap<String, String>,
    weak: HashMap<String, String>,
    overrides: HashMap<String, Option<Vec<String>>>,
    fiction_patterns: Vec<String>,
    pub fingerprint: u64,
}

impl Resolver {
    pub fn new(gazetteer: &Gazetteer) -> Self {
        let mut strong = HashMap::new();
        for (name, _iso, _cluster) in COUNTRIES {
            strong.insert(name.to_lowercase(), name.to_string());
        }
        for (alias, country) in STRONG_ALIASES {
            strong.insert(alias.to_string(), country.to_string());
        }
        for alias in US_ALIASES {
            strong.insert(alias.to_string(), "United States".to_string());
        }
        // Weak entries must not also sit in the strong table, or the precedence rule they exist for
        // would never fire.
        let mut weak = HashMap::new();
        for (alias, country) in WEAK_ALIASES {
            strong.remove(*alias);
            weak.insert(alias.to_string(), country.to_string());
        }

        let mut overrides = HashMap::new();
        for (key, value) in &gazetteer.overrides {
            let normalized = normalize(key);
            match value {
                None => {
                    overrides.insert(normalized, None);
                }
                Some(raw) if raw.trim().is_empty() => {
                    overrides.insert(normalized, None);
                }
                Some(raw) => {
                    let countries: Vec<String> = raw
                        .split(',')
                        .map(|part| part.trim().to_string())
                        .filter(|part| !part.is_empty())
                        .collect();
                    if countries.is_empty() {
                        overrides.insert(normalized, None);
                    } else {
                        overrides.insert(normalized, Some(countries));
                    }
                }
            }
        }

        let mut fiction_patterns: Vec<String> = DEFAULT_FICTION_TITLE_PATTERNS
            .iter()
            .map(|pattern| pattern.to_string())
            .collect();
        for pattern in &gazetteer.fiction_title_patterns {
            let trimmed = pattern.trim().to_lowercase();
            if !trimmed.is_empty() {
                fiction_patterns.push(trimmed);
            }
        }

        Self {
            strong,
            weak,
            overrides,
            fiction_patterns,
            fingerprint: gazetteer_fingerprint(gazetteer),
        }
    }

    /// True when a video title marks the whole group as game or fictional footage.
    pub fn is_fiction_title(&self, title: &str) -> bool {
        let lowered = title.to_lowercase();
        self.fiction_patterns
            .iter()
            .any(|pattern| lowered.contains(pattern.as_str()))
    }

    pub fn resolve(&self, raw: &str) -> Resolution {
        let normalized = normalize(raw);
        if normalized.is_empty() {
            return Resolution::Junk;
        }

        // 1. Hand-written overrides win outright — that is the point of the file.
        if let Some(entry) = self.overrides.get(&normalized) {
            return match entry {
                None => Resolution::Junk,
                Some(countries) => Resolution::Countries(countries.clone()),
            };
        }

        // 2. The model's polite non-answers, article-insensitive.
        let dearticled = normalized
            .strip_prefix("a ")
            .or_else(|| normalized.strip_prefix("an "))
            .or_else(|| normalized.strip_prefix("the "))
            .unwrap_or(&normalized);
        if JUNK_EXACT.iter().any(|entry| dearticled == *entry)
            || JUNK_PREFIXES
                .iter()
                .any(|prefix| dearticled == *prefix || dearticled.starts_with(&format!("{prefix} ")))
        {
            return Resolution::Junk;
        }

        // 3. Exact match on a comma-separated part. "Mumbai, India" and "Sicily, Italy" land here,
        //    and matching whole parts avoids the substring pass's appetite for stray tokens.
        let parts: Vec<&str> = normalized
            .split(',')
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect();
        let mut strong_hits: BTreeSet<String> = BTreeSet::new();
        let mut weak_hits: BTreeSet<String> = BTreeSet::new();
        for part in &parts {
            if let Some(country) = self.strong.get(*part) {
                strong_hits.insert(country.clone());
            } else if let Some(country) = self.weak.get(*part) {
                weak_hits.insert(country.clone());
            }
        }
        if !strong_hits.is_empty() {
            return Resolution::Countries(strong_hits.into_iter().collect());
        }

        // 4. Substring scan. This is what catches routes stated in prose — "on the France to Andorra
        //    route" has no clean comma structure but names both countries.
        for (alias, country) in &self.strong {
            if contains_word(&normalized, alias) {
                strong_hits.insert(country.clone());
            }
        }
        if !strong_hits.is_empty() {
            return Resolution::Countries(strong_hits.into_iter().collect());
        }
        for (alias, country) in &self.weak {
            if contains_word(&normalized, alias) {
                weak_hits.insert(country.clone());
            }
        }
        if !weak_hits.is_empty() {
            return Resolution::Countries(weak_hits.into_iter().collect());
        }

        Resolution::Unresolved
    }
}

/// True when a location line is a ship's port of registry read off its stern rather than the place
/// the footage was shot. Needs both halves: the answer must be the bare port/flag *and* the
/// description must be about a vessel.
pub fn is_registry_port_reading(description: &str, raw: &str) -> bool {
    let normalized = normalize(raw);
    let dearticled = normalized
        .strip_prefix("the ")
        .unwrap_or(&normalized);
    if !REGISTRY_PORTS.iter().any(|port| dearticled == *port) {
        return false;
    }
    let lowered = description.to_lowercase();
    VESSEL_WORDS.iter().any(|word| contains_word(&lowered, word))
}

// ---------------------------------------------------------------------------------------------
// Source identity — collapsing OCR readings of one video title into one video
// ---------------------------------------------------------------------------------------------

/// Fraction of words two title readings must share — after allowing for OCR mangling — to be
/// judged the same video. High on purpose: two videos from one channel differ by only their place
/// name ("[4K] PARIS Aerial Drone Film" / "[4K] LONDON Aerial Drone Film"), and merging *those*
/// would be far worse than leaving a title split, so the margin has to sit above that case.
const TITLE_MERGE_SIMILARITY: f64 = 0.8;
/// Below this many usable tokens a title is too short to identify anything, so it is never merged.
const TITLE_MIN_TOKENS: usize = 3;
/// A token in more groups than this ("driving", "youtube") tells us nothing and would only make the
/// candidate scan quadratic over the whole library.
const TITLE_COMMON_TOKEN_LIMIT: usize = 200;

/// The words a title reading is identified by: four characters or more, no digits.
///
/// Digits are the first thing that varies between readings of one title — YouTube's own "(1)"
/// notification prefix, a "4K60" badge, a year — and the first thing OCR mangles. What survives is
/// the prose.
fn title_tokens(title: &str) -> BTreeSet<String> {
    title
        .split(|ch: char| !ch.is_alphanumeric())
        .filter_map(|word| {
            let lower = word.to_lowercase();
            if lower.chars().count() < 4 || lower.chars().any(|ch| ch.is_ascii_digit()) {
                return None;
            }
            Some(lower)
        })
        .collect()
}

/// Levenshtein distance, abandoned as soon as it provably exceeds `budget`.
fn within_edit_distance(a: &str, b: &str, budget: usize) -> bool {
    let left: Vec<char> = a.chars().collect();
    let right: Vec<char> = b.chars().collect();
    if left.len().abs_diff(right.len()) > budget {
        return false;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (i, lc) in left.iter().enumerate() {
        current[0] = i + 1;
        let mut row_best = current[0];
        for (j, rc) in right.iter().enumerate() {
            let cost = usize::from(lc != rc);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
            row_best = row_best.min(current[j + 1]);
        }
        if row_best > budget {
            return false;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()] <= budget
}

/// Whether two words are the same word as far as a title-bar OCR is concerned.
///
/// The distinction this draws is the whole reason merging is safe: a *mangled* word still looks
/// like its original ("Vlew"/"View", "Railwayto"/"Railway", "Segway"/"Skagway"), whereas a word
/// that genuinely differs between two videos does not ("Paris"/"London"). Everything else here
/// rests on that being able to tell them apart.
fn words_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    // One reading ran two words together or clipped a suffix.
    if short.chars().count() >= 4 && long.starts_with(short) {
        return true;
    }
    // Otherwise a few mangled characters, budgeted against the shorter word's length.
    within_edit_distance(a, b, (short.chars().count() / 3).max(1))
}

/// Whether two title readings are the same video.
///
/// Two conditions, and the second is the one doing the real work. A high shared-word share alone
/// still merges "Tirana to Saranda" with "Tirana to Vlora" — six of seven words agree. What
/// separates them is *where* the disagreement sits: OCR either mangles a word (which
/// [`words_match`] absorbs) or drops one, so its leftovers pile up on ONE side. A word swapped for
/// a different word leaves an orphan on BOTH sides, and that is a different video every time.
fn titles_match(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let right: Vec<&String> = b.iter().collect();
    let mut taken = vec![false; right.len()];
    let mut matched = 0usize;
    for word in a {
        for (index, other) in right.iter().enumerate() {
            if taken[index] || !words_match(word, other) {
                continue;
            }
            taken[index] = true;
            matched += 1;
            break;
        }
    }
    if a.len() - matched > 0 && b.len() - matched > 0 {
        return false;
    }
    (2 * matched) as f64 / (a.len() + b.len()) as f64 >= TITLE_MERGE_SIMILARITY
}

/// Collapses title readings that are the same video into one source, returning `group index ->
/// representative group index`.
///
/// **This is the single biggest correctness fix in the layer.** Grouping keys titles exactly (after
/// stripping punctuation), so every OCR wobble in a title bar splits one video into another
/// "source": measured on the live library, `Mountain Railway to ALASKA | White Pass & Yukon Route`
/// alone occupied seventeen groups — "Rail-way", "Railwayto", "Vlew", "Segway", plus YouTube's own
/// "(1)" prefix appearing on some frames and not others. Since variety here is *counted in sources*,
/// that inflated every diversity number the layer produces: a set could report sixteen videos and
/// show five near-identical frames of one train ride, which is precisely the false prior the whole
/// design exists to avoid.
///
/// Deliberately conservative — a high overlap threshold, digits ignored, short titles never merged —
/// because merging two genuinely different videos costs more than leaving one split.
pub fn canonical_groups(titles: &[&str]) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..titles.len()).collect();
    fn find(parent: &mut Vec<usize>, mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }

    let tokens: Vec<BTreeSet<String>> = titles.iter().map(|title| title_tokens(title)).collect();

    // Inverted index: only titles sharing at least one distinctive word are ever compared.
    let mut buckets: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, set) in tokens.iter().enumerate() {
        if set.len() < TITLE_MIN_TOKENS {
            continue;
        }
        for token in set {
            buckets.entry(token.as_str()).or_default().push(index);
        }
    }

    for (_token, candidates) in &buckets {
        if candidates.len() > TITLE_COMMON_TOKEN_LIMIT {
            continue;
        }
        for (offset, left) in candidates.iter().enumerate() {
            for right in candidates.iter().skip(offset + 1) {
                if find(&mut parent, *left) == find(&mut parent, *right) {
                    continue;
                }
                if titles_match(&tokens[*left], &tokens[*right]) {
                    let a = find(&mut parent, *left);
                    let b = find(&mut parent, *right);
                    parent[b] = a;
                }
            }
        }
    }

    (0..titles.len()).map(|index| find(&mut parent, index)).collect()
}

/// Pulls the `Location:` line the describe prompt asks for out of a description.
pub fn extract_location_line(description: &str) -> Option<String> {
    for line in description.lines() {
        let trimmed = line.trim();
        // The model sometimes writes the marker mid-line after a sentence; take everything after it.
        if let Some(index) = trimmed.to_lowercase().find("location:") {
            let value = trimmed[index + "location:".len()..].trim();
            let value = value.trim_end_matches('.').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Derived records
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoRecord {
    /// More than one entry = a route video crossing a border. The image belongs to every one of
    /// these country sets.
    pub countries: Vec<String>,
    /// The `Location:` string this tag came from — this frame's own, or a sampled frame of its video.
    /// Kept verbatim: it is the audit trail, and it is also the only place the sub-national detail
    /// ("Skagway, Alaska") survives for a future admin-level breakdown.
    pub raw: String,
    /// `own` = this image's own description. `group` = propagated from its video's sampled frames.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_group: Option<usize>,
    /// `high` when the image speaks for itself or its whole video agreed; `medium` when propagated
    /// from samples that disagreed.
    pub confidence: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoStats {
    pub described: usize,
    pub with_location: usize,
    pub tagged_own: usize,
    pub tagged_propagated: usize,
    pub tagged_total: usize,
    pub rejected_junk: usize,
    /// Location lines that were a ship's port of registry rather than a place — see
    /// [`is_registry_port_reading`].
    #[serde(default)]
    pub rejected_registry_port: usize,
    pub unresolved_images: usize,
    pub unresolved_strings: usize,
    pub fiction_groups_skipped: usize,
    pub countries_seen: usize,
    /// Distinct videos after collapsing OCR readings of one title — the real denominator behind
    /// every "N videos" figure in the app.
    #[serde(default)]
    pub sources: usize,
    /// How many chunk-plan groups those videos were spread across before merging. The gap is pure
    /// OCR jitter, and it is what used to be counted as variety.
    #[serde(default)]
    pub source_groups: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoFile {
    pub version: u32,
    pub generated_at: String,
    pub gazetteer_fingerprint: String,
    pub stats: GeoStats,
    pub images: BTreeMap<String, GeoRecord>,
    /// Location strings this derive could not place, and how many images each accounts for. Also
    /// mirrored into the gazetteer as the hand-editable worklist — kept here too so the coverage
    /// view stays a pure function of this file rather than silently emptying when the gazetteer on
    /// disk is older than the records.
    #[serde(default)]
    pub unresolved: BTreeMap<String, u32>,
    /// Country -> distinct video sources, as of this derive.
    pub coverage: BTreeMap<String, usize>,
    /// The same map from the *previous* derive, so the coverage view can show movement without a
    /// fourth sidecar file. This is what turns the scoreboard into feedback for the capture loop.
    #[serde(default)]
    pub previous_coverage: BTreeMap<String, usize>,
}

pub fn geo_path(root: &Path) -> PathBuf {
    root.join(GEO_FILE_NAME)
}

pub fn load_geo(root: &Path) -> Option<GeoFile> {
    std::fs::read_to_string(geo_path(root))
        .ok()
        .and_then(|text| serde_json::from_str::<GeoFile>(&text).ok())
}

/// One described image handed to the derive: its hash and the prose the vision model produced.
pub struct DescribedImage {
    pub hash: String,
    pub description: String,
}

/// One video group from the chunk plan.
pub struct SourceGroup<'a> {
    pub title: &'a str,
    pub member_hashes: &'a [String],
}

/// Rebuilds the whole geo layer. Pure over its inputs, so it is safe to run after every scan.
///
/// `now` is passed in rather than read from the clock so the derive stays deterministic and testable.
pub fn derive(
    descriptions: &[DescribedImage],
    groups: &[SourceGroup<'_>],
    gazetteer: &mut Gazetteer,
    previous: Option<&GeoFile>,
    now: String,
) -> GeoFile {
    let resolver = Resolver::new(gazetteer);

    // One video, one source — however many ways OCR read its title bar. Everything downstream
    // (propagation, variety counts, set building) works on these merged sources, never on raw plan
    // groups, because a raw group is a *title reading* and variety has to be counted in videos.
    let titles: Vec<&str> = groups.iter().map(|group| group.title).collect();
    let canonical = canonical_groups(&titles);
    // Within each cluster the group with the most frames speaks for it: its index is what records
    // record, its title is what the fiction denylist and the `via` audit trail see.
    let mut representative: HashMap<usize, usize> = HashMap::new();
    for (index, root) in canonical.iter().enumerate() {
        let entry = representative.entry(*root).or_insert(index);
        if groups[index].member_hashes.len() > groups[*entry].member_hashes.len() {
            *entry = index;
        }
    }
    // Cluster root -> (representative group index, every member hash in the cluster).
    let mut clusters: Vec<(usize, Vec<&str>)> = Vec::new();
    let mut cluster_of_root: HashMap<usize, usize> = HashMap::new();
    for (index, root) in canonical.iter().enumerate() {
        let slot = *cluster_of_root.entry(*root).or_insert_with(|| {
            clusters.push((representative[root], Vec::new()));
            clusters.len() - 1
        });
        for hash in groups[index].member_hashes {
            clusters[slot].1.push(hash.as_str());
        }
    }

    let mut hash_to_cluster: HashMap<&str, usize> = HashMap::new();
    for (index, (_representative, members)) in clusters.iter().enumerate() {
        for hash in members {
            hash_to_cluster.insert(hash, index);
        }
    }

    let mut stats = GeoStats {
        described: descriptions.len(),
        source_groups: groups.len(),
        sources: clusters.len(),
        ..GeoStats::default()
    };
    let mut unresolved: BTreeMap<String, u32> = BTreeMap::new();
    let mut records: BTreeMap<String, GeoRecord> = BTreeMap::new();
    // What each described frame resolved to, so group propagation can reuse it.
    let mut own: HashMap<&str, (Vec<String>, String)> = HashMap::new();

    for image in descriptions {
        let Some(raw) = extract_location_line(&image.description) else {
            continue;
        };
        stats.with_location += 1;
        // A flag of convenience read off a stern is not a location. Rejected before resolution so
        // the reading never seeds its group either — one such frame used to tag a whole video, and
        // through propagation an entire country.
        if is_registry_port_reading(&image.description, &raw) {
            stats.rejected_registry_port += 1;
            continue;
        }
        match resolver.resolve(&raw) {
            Resolution::Countries(countries) => {
                own.insert(image.hash.as_str(), (countries.clone(), raw.clone()));
            }
            Resolution::Junk => stats.rejected_junk += 1,
            Resolution::Unresolved => {
                stats.unresolved_images += 1;
                *unresolved.entry(raw.to_lowercase()).or_insert(0) += 1;
            }
        }
    }

    // Own-description tags first — they always beat a propagated guess.
    for image in descriptions {
        if let Some((countries, raw)) = own.get(image.hash.as_str()) {
            let cluster = hash_to_cluster.get(image.hash.as_str()).copied();
            let group_index = cluster.map(|index| clusters[index].0);
            // A frame from a fictional video is fictional even when it describes a real-looking place.
            if let Some(index) = group_index {
                if resolver.is_fiction_title(groups[index].title) {
                    continue;
                }
            }
            records.insert(
                image.hash.clone(),
                GeoRecord {
                    countries: countries.clone(),
                    raw: raw.clone(),
                    source: "own".to_string(),
                    via: None,
                    source_group: group_index,
                    confidence: "high".to_string(),
                },
            );
        }
    }
    stats.tagged_own = records.len();

    // Then propagate each video's resolved countries onto its unsampled frames. Merged sources pay
    // off twice here: a reading that got its own fragment group now inherits from the sampled
    // frames of the rest of its video instead of going untagged.
    for (index, members) in clusters.iter() {
        let index = *index;
        let group = &groups[index];
        if resolver.is_fiction_title(group.title) {
            stats.fiction_groups_skipped += 1;
            continue;
        }
        let mut countries: BTreeSet<String> = BTreeSet::new();
        let mut representative: Option<String> = None;
        let mut distinct_answers = 0usize;
        for hash in members {
            if let Some((resolved, raw)) = own.get(*hash) {
                distinct_answers += 1;
                if representative.is_none() {
                    representative = Some(raw.clone());
                }
                for country in resolved {
                    countries.insert(country.clone());
                }
            }
        }
        if countries.is_empty() {
            continue;
        }
        // One country across every sampled frame is a confident read of the whole video. A spread
        // means either a genuine border crossing or a shaky sample; either way the union is right,
        // but it should not claim the same confidence.
        let confidence = if countries.len() == 1 && distinct_answers > 0 {
            "high"
        } else {
            "medium"
        };
        let countries: Vec<String> = countries.into_iter().collect();
        let raw = representative.unwrap_or_default();
        for hash in members {
            if records.contains_key(*hash) {
                continue;
            }
            records.insert(
                (*hash).to_string(),
                GeoRecord {
                    countries: countries.clone(),
                    raw: raw.clone(),
                    source: "group".to_string(),
                    via: Some(group.title.to_string()),
                    source_group: Some(index),
                    confidence: confidence.to_string(),
                },
            );
        }
    }

    stats.tagged_total = records.len();
    stats.tagged_propagated = stats.tagged_total.saturating_sub(stats.tagged_own);
    stats.unresolved_strings = unresolved.len();

    let coverage = source_counts(&records);
    stats.countries_seen = coverage.len();

    // The worklist is regenerated wholesale; the hand-written overrides are untouched.
    gazetteer.unresolved = unresolved.clone();
    if gazetteer.note.is_empty() {
        gazetteer.note = GAZETTEER_NOTE.to_string();
    }

    GeoFile {
        version: GEO_SCHEMA_VERSION,
        generated_at: now,
        gazetteer_fingerprint: format!("{:016x}", resolver.fingerprint),
        stats,
        images: records,
        unresolved,
        coverage,
        previous_coverage: previous.map(|file| file.coverage.clone()).unwrap_or_default(),
    }
}

/// The source key an image counts under for variety purposes: its video, or itself when it is a
/// standalone screenshot. Two frames of one video are one source no matter how different they look.
fn source_key(hash: &str, record: &GeoRecord) -> String {
    match record.source_group {
        Some(index) => format!("g{index}"),
        None => format!("s{hash}"),
    }
}

/// Country -> number of distinct video sources.
pub fn source_counts(records: &BTreeMap<String, GeoRecord>) -> BTreeMap<String, usize> {
    let mut sources: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (hash, record) in records {
        let key = source_key(hash, record);
        for country in &record.countries {
            sources
                .entry(country.clone())
                .or_default()
                .insert(key.clone());
        }
    }
    sources
        .into_iter()
        .map(|(country, keys)| (country, keys.len()))
        .collect()
}

/// Country -> number of images.
pub fn image_counts(records: &BTreeMap<String, GeoRecord>) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for record in records.values() {
        for country in &record.countries {
            *counts.entry(country.clone()).or_insert(0) += 1;
        }
    }
    counts
}

// ---------------------------------------------------------------------------------------------
// Coverage view
// ---------------------------------------------------------------------------------------------

pub fn tier_for(sources: usize) -> &'static str {
    if sources == 0 {
        "empty"
    } else if sources <= SEED_CEILING {
        "seed"
    } else if sources < DIVERSITY_FLOOR {
        "thin"
    } else if sources < DEEP_FLOOR {
        "ready"
    } else {
        "deep"
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CountryCoverage {
    pub name: String,
    pub iso2: String,
    pub images: usize,
    pub sources: usize,
    pub tier: String,
    /// Change in sources since the previous derive — the capture loop's feedback signal.
    pub delta: i64,
    /// How many genuinely varied sets this country can currently fill.
    pub buildable_sets: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterCoverage {
    pub name: String,
    pub ready: usize,
    pub total: usize,
    pub countries: Vec<CountryCoverage>,
}

/// One line of the gazetteer worklist: a location string the resolver could not place, and how many
/// images placing it would tag. Sorted by payoff so the top of the list is always the best next fix.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorklistEntry {
    pub location: String,
    pub images: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageView {
    pub generated_at: String,
    pub clusters: Vec<ClusterCoverage>,
    pub tiers: BTreeMap<String, usize>,
    pub stats: GeoStats,
    /// Highest-payoff unresolved strings, straight from the gazetteer.
    pub worklist: Vec<WorklistEntry>,
    /// Countries tagged that are not in the reference list — usually a gazetteer override naming a
    /// country outside the 109, worth surfacing rather than silently dropping.
    pub off_reference: Vec<String>,
    pub gazetteer_path: String,
    pub total_reference: usize,
}

pub fn coverage_view(root: &Path, geo: &GeoFile) -> CoverageView {
    let images = image_counts(&geo.images);
    let mut tiers: BTreeMap<String, usize> = BTreeMap::new();
    let mut clusters: Vec<ClusterCoverage> = Vec::new();
    // On the very first derive there is nothing to compare against, and reporting every country as
    // a gain would read as "you just captured all of this" on a run that captured nothing.
    let has_baseline = !geo.previous_coverage.is_empty();

    for cluster_name in CLUSTER_ORDER {
        let mut countries: Vec<CountryCoverage> = Vec::new();
        for (name, iso2, cluster) in COUNTRIES {
            if cluster != cluster_name {
                continue;
            }
            let sources = geo.coverage.get(*name).copied().unwrap_or(0);
            let previous = geo.previous_coverage.get(*name).copied().unwrap_or(0);
            let tier = tier_for(sources).to_string();
            *tiers.entry(tier.clone()).or_insert(0) += 1;
            countries.push(CountryCoverage {
                name: name.to_string(),
                iso2: iso2.to_string(),
                images: images.get(*name).copied().unwrap_or(0),
                sources,
                tier,
                delta: if has_baseline { sources as i64 - previous as i64 } else { 0 },
                buildable_sets: sources / DIVERSITY_FLOOR,
            });
        }
        // Worst first: the view exists to show gaps, so the countries needing capture lead.
        countries.sort_by(|a, b| a.sources.cmp(&b.sources).then_with(|| a.name.cmp(&b.name)));
        let ready = countries
            .iter()
            .filter(|country| country.sources >= DIVERSITY_FLOOR)
            .count();
        let total = countries.len();
        clusters.push(ClusterCoverage {
            name: cluster_name.to_string(),
            ready,
            total,
            countries,
        });
    }

    // Biggest payoff first — the top of this list is always the best next gazetteer edit.
    let mut worklist: Vec<WorklistEntry> = geo
        .unresolved
        .iter()
        .map(|(location, images)| WorklistEntry {
            location: location.clone(),
            images: *images,
        })
        .collect();
    worklist.sort_by(|a, b| b.images.cmp(&a.images).then_with(|| a.location.cmp(&b.location)));
    worklist.truncate(WORKLIST_LIMIT);

    let reference: HashSet<&str> = COUNTRIES.iter().map(|(name, _, _)| *name).collect();
    let mut off_reference: Vec<String> = geo
        .coverage
        .keys()
        .filter(|name| !reference.contains(name.as_str()))
        .cloned()
        .collect();
    off_reference.sort();

    CoverageView {
        generated_at: geo.generated_at.clone(),
        clusters,
        tiers,
        stats: geo.stats.clone(),
        worklist,
        off_reference,
        gazetteer_path: gazetteer_path(root).to_string_lossy().to_string(),
        total_reference: COUNTRIES.len(),
    }
}

// ---------------------------------------------------------------------------------------------
// Sets
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoSet {
    pub id: String,
    pub kind: String,
    pub country: String,
    pub title: String,
    pub size: usize,
    /// Distinct videos the members came from. The number that actually predicts whether the set is
    /// worth practising on.
    pub sources: usize,
    /// How many frames had to be taken from a single video to fill the set. 1 is ideal.
    pub max_per_source: usize,
    /// `diverse` when every member came from a different video, `limited` otherwise. A limited set
    /// is still useful as reference — "this is what Liberia looks like" — but it is not practice.
    pub quality: String,
    pub members: Vec<String>,
    pub gazetteer_fingerprint: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoSetsFile {
    pub version: u32,
    pub generated_at: String,
    pub target_size: usize,
    pub sets: Vec<GeoSet>,
}

pub fn sets_path(root: &Path) -> PathBuf {
    root.join(GEO_SETS_FILE_NAME)
}

/// One hand-excluded image. The name and timestamp are for the human reading the file — matching is
/// purely by the hash key, same as everything else in this layer.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoExclusion {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub excluded_at: String,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoExcludedFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub note: String,
    /// hash -> why/when. Delete a line here to let an image back into set building.
    #[serde(default)]
    pub excluded: BTreeMap<String, GeoExclusion>,
}

pub fn excluded_path(root: &Path) -> PathBuf {
    root.join(GEO_EXCLUDED_FILE_NAME)
}

pub fn load_excluded(root: &Path) -> GeoExcludedFile {
    std::fs::read_to_string(excluded_path(root))
        .ok()
        .and_then(|text| serde_json::from_str::<GeoExcludedFile>(&text).ok())
        .unwrap_or_default()
}

pub fn load_sets(root: &Path) -> Option<GeoSetsFile> {
    std::fs::read_to_string(sets_path(root))
        .ok()
        .and_then(|text| serde_json::from_str::<GeoSetsFile>(&text).ok())
}

/// Stable pseudo-random ordering key. Same inputs always give the same set, so rebuilding without
/// changing anything does not reshuffle sets the user has already looked at.
fn shuffle_key(seed: &str, value: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
}

/// Builds country sets from the derived records.
///
/// Members are drawn one per video before any video is used twice, and the per-video allowance is
/// only raised when the country cannot otherwise fill a set. That keeps a rich country's sets fully
/// varied while still producing something openable for a country that only has one or two videos —
/// badged `limited` so the difference is never invisible.
/// `kinds` maps hash -> scene kind; `allowed` is the set of kinds a member may have. An image with
/// no kind yet is **kept** — the classifier is an optional pass, and silently emptying every set
/// until it has run would be a worse failure than letting a few interiors through.
pub fn build_sets(
    geo: &GeoFile,
    target_size: usize,
    excluded: &BTreeMap<String, GeoExclusion>,
    kinds: &BTreeMap<String, String>,
    allowed: &[String],
    now: String,
) -> GeoSetsFile {
    let target_size = target_size.max(1);
    let mut by_country: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for (hash, record) in &geo.images {
        // Hand-excluded images never enter the pool, so a rebuild can't quietly reinstate one.
        if excluded.contains_key(hash) {
            continue;
        }
        // A classified image must be an allowed kind; an unclassified one passes through.
        if let Some(kind) = kinds.get(hash) {
            if !allowed.iter().any(|value| value == kind) {
                continue;
            }
        }
        let key = source_key(hash, record);
        for country in &record.countries {
            by_country
                .entry(country.clone())
                .or_default()
                .entry(key.clone())
                .or_default()
                .push(hash.clone());
        }
    }

    let mut sets: Vec<GeoSet> = Vec::new();
    for (country, sources) in &by_country {
        // Deterministic ordering: sources shuffled by a stable key, frames within a source likewise.
        let mut source_keys: Vec<&String> = sources.keys().collect();
        source_keys.sort_by_key(|key| shuffle_key(country, key));

        let total_sources = source_keys.len();
        let set_count = (total_sources / target_size).max(1);

        for set_index in 0..set_count {
            // Non-overlapping source slices while the country is rich enough to afford them; a thin
            // country falls back to its whole pool for its single set.
            let slice: Vec<&&String> = if total_sources >= target_size {
                source_keys
                    .iter()
                    .skip(set_index * target_size)
                    .take(target_size)
                    .collect()
            } else {
                source_keys.iter().collect()
            };
            if slice.is_empty() {
                continue;
            }

            let mut members: Vec<String> = Vec::new();
            let mut used_sources: BTreeSet<String> = BTreeSet::new();
            let mut per_source = 0usize;
            // Round after round, allowing one more frame per video each time, until the set is full
            // or the pool is exhausted.
            while members.len() < target_size {
                per_source += 1;
                let before = members.len();
                for key in &slice {
                    if members.len() >= target_size {
                        break;
                    }
                    let mut frames = sources.get(**key).cloned().unwrap_or_default();
                    frames.sort_by_key(|hash| shuffle_key(country, hash));
                    if let Some(hash) = frames.get(per_source - 1) {
                        members.push(hash.clone());
                        used_sources.insert((**key).clone());
                    }
                }
                if members.len() == before {
                    break; // pool exhausted
                }
            }
            if members.is_empty() {
                continue;
            }

            let achieved_max = if used_sources.is_empty() {
                0
            } else {
                // Ceiling division: the largest number of frames any one video contributed.
                (members.len() + used_sources.len() - 1) / used_sources.len()
            };
            let quality = if used_sources.len() >= target_size && achieved_max <= 1 {
                "diverse"
            } else {
                "limited"
            };
            let title = if set_count > 1 {
                format!("{country} · {}", set_index + 1)
            } else {
                country.clone()
            };
            sets.push(GeoSet {
                id: format!("country:{}:{}", country.to_lowercase().replace(' ', "-"), set_index + 1),
                kind: "country".to_string(),
                country: country.clone(),
                title,
                size: members.len(),
                sources: used_sources.len(),
                max_per_source: achieved_max,
                quality: quality.to_string(),
                members,
                gazetteer_fingerprint: geo.gazetteer_fingerprint.clone(),
                generated_at: now.clone(),
            });
        }
    }

    // Best material first, then alphabetical. No id tiebreak: `sort_by` is stable, so sets of one
    // country keep their build order — comparing ids as strings would file "Brazil - 10" before
    // "Brazil - 2".
    sets.sort_by(|a, b| {
        b.sources
            .cmp(&a.sources)
            .then_with(|| a.country.cmp(&b.country))
    });

    GeoSetsFile {
        version: GEO_SCHEMA_VERSION,
        generated_at: now,
        target_size,
        sets,
    }
}

// ---------------------------------------------------------------------------------------------
// Freshness
//
// The Geo panel shows three vintages side by side — records, sets, and the exclusion list — written
// at three different moments by three different actions, one of them (`.image-categorizer-geo-
// excluded.json`) by ANOTHER APP entirely. A panel that simply paints what it last read is therefore
// showing a mixture with nothing on screen saying so, and it fails silently in both directions:
// coverage newer than the sets beside it, and sets still holding images excluded an hour ago.
//
// Exactness where it is available, ordering where it is not. "An excluded image is still a set
// member" is an intersection and cannot be wrong; "the gazetteer was edited after the last derive"
// is a question about write ORDER, and the files' own timestamps are the honest answer to it. No
// fourth store is introduced for any of this — everything compared here is already on disk.
// ---------------------------------------------------------------------------------------------

/// Everything the freshness check compares, gathered by the caller so this stays a pure function.
pub struct StatusInput<'a> {
    /// `None` when no sets have been built yet.
    pub sets: Option<&'a GeoSetsFile>,
    pub excluded: &'a BTreeMap<String, GeoExclusion>,
    /// hash -> effective scene kind: exactly the map `build_sets` filtered against.
    pub kinds: &'a BTreeMap<String, String>,
    pub allowed_kinds: &'a [String],
    /// Sidecar write times in epoch seconds, `None` when the file does not exist. Only their
    /// relative order is ever read.
    pub derived_at: Option<u64>,
    pub gazetteer_at: Option<u64>,
    pub sets_at: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoStatus {
    pub has_records: bool,
    pub has_sets: bool,
    pub sets_count: usize,
    pub set_members: usize,
    pub target_size: usize,
    /// Straight out of the sets file, so the panel can date the sets independently of the records.
    pub sets_built_at: Option<String>,
    pub excluded_total: usize,
    /// Hand-excluded images STILL inside a built set — proof the sets predate the exclusion.
    pub excluded_in_sets: usize,
    /// Members since classified as a kind sets are not allowed to draw from.
    pub disallowed_in_sets: usize,
    pub records_newer_than_sets: bool,
    pub gazetteer_newer_than_records: bool,
    /// The gazetteer holds decisions the records have never been through.
    pub records_stale: bool,
    /// The sets no longer match the records, exclusions or kinds they were built from.
    pub sets_stale: bool,
    /// One sentence per problem, each naming the button that fixes it. Empty = the files agree.
    pub reasons: Vec<String>,
}

pub fn status(input: &StatusInput<'_>) -> GeoStatus {
    let mut excluded_in_sets: BTreeSet<&str> = BTreeSet::new();
    let mut disallowed_in_sets: BTreeSet<&str> = BTreeSet::new();
    let mut set_members = 0usize;

    for set in input.sets.iter().flat_map(|file| file.sets.iter()) {
        set_members += set.members.len();
        for hash in &set.members {
            if input.excluded.contains_key(hash) {
                excluded_in_sets.insert(hash.as_str());
            }
            // An unclassified member passes through `build_sets`, so only a member that HAS a kind
            // and is not an allowed one counts as drift. Anything stricter would report every set
            // as broken on a library that has never run the scene pass.
            if let Some(kind) = input.kinds.get(hash) {
                if !input.allowed_kinds.iter().any(|allowed| allowed == kind) {
                    disallowed_in_sets.insert(hash.as_str());
                }
            }
        }
    }

    let has_records = input.derived_at.is_some();
    let has_sets = input.sets.is_some_and(|file| !file.sets.is_empty());
    // Strictly greater, and `derive` writes the gazetteer just before the records: a derive can
    // never accuse itself of leaving the gazetteer unapplied.
    let gazetteer_newer_than_records = has_records && input.gazetteer_at > input.derived_at;
    let records_newer_than_sets = has_sets && input.derived_at > input.sets_at;

    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut reasons: Vec<String> = Vec::new();
    if gazetteer_newer_than_records {
        reasons.push(
            "The gazetteer has been edited since the last derive, so the coverage below does not \
             include those decisions yet — press Derive Geo."
                .to_string(),
        );
    }
    if records_newer_than_sets {
        reasons.push(
            "These sets were built from an earlier derive than the coverage beside them — press \
             Build Country Sets to rebuild them from the current records."
                .to_string(),
        );
    }
    if !excluded_in_sets.is_empty() {
        reasons.push(format!(
            "{} still {} of a built set — press Build Country Sets to drop {}.",
            count(excluded_in_sets.len(), "hand-excluded image is", "hand-excluded images are"),
            if excluded_in_sets.len() == 1 { "a member" } else { "members" },
            if excluded_in_sets.len() == 1 { "it" } else { "them" },
        ));
    }
    if !disallowed_in_sets.is_empty() {
        reasons.push(format!(
            "{} since been classified as a scene kind sets may not use (allowed: {}) — press \
             Build Country Sets to drop {}.",
            count(disallowed_in_sets.len(), "set member has", "set members have"),
            if input.allowed_kinds.is_empty() {
                "none".to_string()
            } else {
                input.allowed_kinds.join(", ")
            },
            if disallowed_in_sets.len() == 1 { "it" } else { "them" },
        ));
    }

    GeoStatus {
        has_records,
        has_sets,
        sets_count: input.sets.map(|file| file.sets.len()).unwrap_or(0),
        set_members,
        target_size: input.sets.map(|file| file.target_size).unwrap_or(0),
        sets_built_at: input
            .sets
            .map(|file| file.generated_at.clone())
            .filter(|value| !value.is_empty()),
        excluded_total: input.excluded.len(),
        excluded_in_sets: excluded_in_sets.len(),
        disallowed_in_sets: disallowed_in_sets.len(),
        records_newer_than_sets,
        gazetteer_newer_than_records,
        records_stale: gazetteer_newer_than_records,
        sets_stale: records_newer_than_sets
            || !excluded_in_sets.is_empty()
            || !disallowed_in_sets.is_empty(),
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver() -> Resolver {
        Resolver::new(&Gazetteer::default())
    }

    #[test]
    fn resolves_country_and_city_country_forms() {
        let r = resolver();
        assert_eq!(r.resolve("Germany"), Resolution::Countries(vec!["Germany".into()]));
        assert_eq!(r.resolve("Mumbai, India"), Resolution::Countries(vec!["India".into()]));
        assert_eq!(r.resolve("Sicily, Italy"), Resolution::Countries(vec!["Italy".into()]));
    }

    #[test]
    fn resolves_bare_city_and_region_without_a_country() {
        let r = resolver();
        assert_eq!(r.resolve("Istanbul"), Resolution::Countries(vec!["Turkey".into()]));
        assert_eq!(r.resolve("Yorkshire Dales"), Resolution::Countries(vec!["United Kingdom".into()]));
        assert_eq!(r.resolve("Bangkok"), Resolution::Countries(vec!["Thailand".into()]));
    }

    #[test]
    fn route_prose_yields_both_countries() {
        let r = resolver();
        let resolved = r.resolve("Pyrenees mountains, on the France to Andorra route");
        match resolved {
            Resolution::Countries(countries) => {
                assert!(countries.contains(&"France".to_string()));
                assert!(countries.contains(&"Andorra".to_string()));
            }
            other => panic!("expected a route, got {other:?}"),
        }
    }

    #[test]
    fn strong_match_beats_an_ambiguous_one() {
        // "Georgia" alone is the country; next to a US city it is the state, and the whole point of
        // the weak table is that the second case must not produce Caucasus footage.
        let r = resolver();
        assert_eq!(r.resolve("Georgia"), Resolution::Countries(vec!["Georgia".into()]));
        assert_eq!(
            r.resolve("Atlanta, Georgia"),
            Resolution::Countries(vec!["United States".into()])
        );
    }

    #[test]
    fn non_geographic_answers_are_junk_not_worklist() {
        let r = resolver();
        assert_eq!(r.resolve("A marble quarry"), Resolution::Junk);
        assert_eq!(r.resolve("Not specified"), Resolution::Junk);
        assert_eq!(r.resolve("a forest clearing"), Resolution::Junk);
        // The model is inconsistent about articles; all three forms must reject together or each
        // variant sits in the worklist as its own unfixable line.
        assert_eq!(r.resolve("military truck factory"), Resolution::Junk);
        assert_eq!(r.resolve("The marble quarry"), Resolution::Junk);
        assert_eq!(r.resolve("N/A"), Resolution::Junk);
        // ...but a generic noun must not swallow a real place that merely begins with it.
        assert_eq!(
            r.resolve("Port of Rotterdam, Netherlands"),
            Resolution::Countries(vec!["Netherlands".into()])
        );
        assert_eq!(r.resolve("Port"), Resolution::Junk);
    }

    #[test]
    fn unplaceable_but_real_looking_goes_to_the_worklist() {
        let r = resolver();
        assert_eq!(r.resolve("Vitoria-Minas Railway"), Resolution::Countries(vec!["Brazil".into()]));
        assert_eq!(r.resolve("Zzyzx Sector 9"), Resolution::Unresolved);
    }

    #[test]
    fn overrides_win_and_support_routes_and_rejection() {
        let mut gazetteer = Gazetteer::default();
        gazetteer.overrides.insert("apache canyon".into(), Some("United States".into()));
        gazetteer.overrides.insert("the hidden valley".into(), None);
        gazetteer.overrides.insert("border run".into(), Some("Finland, Sweden".into()));
        let r = Resolver::new(&gazetteer);
        assert_eq!(r.resolve("Apache Canyon"), Resolution::Countries(vec!["United States".into()]));
        assert_eq!(r.resolve("The Hidden Valley"), Resolution::Junk);
        assert_eq!(
            r.resolve("Border run"),
            Resolution::Countries(vec!["Finland".into(), "Sweden".into()])
        );
    }

    #[test]
    fn worklist_decisions_write_the_same_file_a_hand_edit_would() {
        let dir = std::env::temp_dir().join(format!("icat-override-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut seeded = Gazetteer::default();
        seeded.fiction_title_patterns.push("exploring empty maps".into());
        seeded.overrides.insert("kept by hand".into(), Some("Finland".into()));
        save_gazetteer(&dir, &seeded).unwrap();

        // Case, surrounding whitespace and a sloppy route separator all normalize on the way in, so
        // the stored line is indistinguishable from one typed into the file.
        set_override(&dir, "  Skagway, Alaska ", "place", Some(" United States ")).unwrap();
        set_override(&dir, "Border Run", "place", Some("Finland ,, Sweden,")).unwrap();
        set_override(&dir, "A Marble Quarry", "reject", None).unwrap();

        let after = load_gazetteer(&dir);
        assert_eq!(after.overrides.get("skagway, alaska"), Some(&Some("United States".to_string())));
        assert_eq!(after.overrides.get("border run"), Some(&Some("Finland, Sweden".to_string())));
        assert_eq!(after.overrides.get("a marble quarry"), Some(&None));
        assert_eq!(
            after.overrides.get("kept by hand"),
            Some(&Some("Finland".to_string())),
            "an unrelated hand-written line must survive a decision made from the UI"
        );
        assert_eq!(after.fiction_title_patterns, vec!["exploring empty maps".to_string()]);

        // And the resolver reads back exactly what the UI wrote — the point of the whole loop.
        let resolver = Resolver::new(&after);
        assert_eq!(
            resolver.resolve("Skagway, Alaska"),
            Resolution::Countries(vec!["United States".into()])
        );
        assert_eq!(
            resolver.resolve("Border run"),
            Resolution::Countries(vec!["Finland".into(), "Sweden".into()])
        );
        assert_eq!(resolver.resolve("A marble quarry"), Resolution::Junk);

        // Undo deletes the line rather than recording a decision of its own.
        set_override(&dir, "Skagway, Alaska", "clear", None).unwrap();
        assert!(!load_gazetteer(&dir).overrides.contains_key("skagway, alaska"));

        // A place with nothing typed in it must not be stored as a rejection by accident.
        assert!(set_override(&dir, "Somewhere", "place", Some("  ,  ")).is_err());
        assert!(set_override(&dir, "   ", "reject", None).is_err());
        assert!(set_override(&dir, "Somewhere", "sideways", None).is_err());
        assert!(!load_gazetteer(&dir).overrides.contains_key("somewhere"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_registry_port_only_rejects_when_there_is_a_ship_in_frame() {
        let ship = "A container ship rounds a tight river bend, MONROVIA painted on her stern.";
        let street = "A busy street in Monrovia with market stalls along the roadside.";
        assert!(is_registry_port_reading(ship, "Monrovia"));
        assert!(is_registry_port_reading(ship, "Panama"));
        // No vessel in the description: the place is taken at its word.
        assert!(!is_registry_port_reading(street, "Monrovia"));
        // A real place that merely contains the flag word survives even with ships around.
        assert!(!is_registry_port_reading(ship, "Panama Canal"));
        assert!(!is_registry_port_reading(ship, "Colón, Panama"));
    }

    #[test]
    fn ocr_readings_of_one_title_collapse_into_one_source() {
        // Every one of these is the same video: YouTube's "(1)" prefix comes and goes, a badge is
        // dropped, and OCR mangles a letter or two. Exact keying filed them as five videos, which
        // is five times the variety the library actually has.
        let titles = vec![
            "(1) Mountain Railway to ALASKA I White Pass & Yukon Route Driver's View to Skagway",
            "Mountain Railway to ALASKA | White Pass & Yukon Route Driver's View to Skagway",
            "(1) 4K60 Mountain Rail-way to ALASKA I White Pass & Yukon Route Driver's Vlew to Skagway",
            "(1) Mountain Railwayto ALASKA I White pass & Yukon Route Driver•s View to Segway",
            "Walking in Kyoto at night - a completely different video",
        ];
        let canonical = canonical_groups(&titles);
        assert_eq!(canonical[0], canonical[1]);
        assert_eq!(canonical[0], canonical[2]);
        assert_eq!(canonical[0], canonical[3]);
        assert_ne!(canonical[0], canonical[4], "an unrelated title must not be swallowed");
    }

    #[test]
    fn two_videos_from_one_series_are_not_merged() {
        // The failure mode that decides the threshold: same channel, same wording, different
        // country. Merging these would hand one video's frames to the other's country.
        let titles = vec![
            "[4K] PARIS 2025 Aerial Drone Relaxation Film UHD FRANCE",
            "[4K] LONDON 2025 Aerial Drone Relaxation Film UHD ENGLAND",
            "Driving from Tirana to Saranda Albania real time",
            "Driving from Tirana to Vlora Albania real time",
        ];
        let canonical = canonical_groups(&titles);
        assert_ne!(canonical[0], canonical[1], "different country, same series");
        assert_ne!(canonical[2], canonical[3], "different route, same series");
    }

    #[test]
    fn merged_titles_count_as_one_video_and_propagate_across_fragments() {
        // Two fragment groups of one video; only the first has a described frame.
        let a: Vec<String> = (0..4).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..3).map(|i| format!("b{i}")).collect();
        let descriptions = vec![DescribedImage {
            hash: "a0".into(),
            description: "Location: Kyoto, Japan".into(),
        }];
        let groups = vec![
            SourceGroup { title: "Mountain Railway to Kyoto Driver's View", member_hashes: &a },
            SourceGroup { title: "(1) Mountain Rail-way to Kyoto Driver's Vlew", member_hashes: &b },
        ];
        let mut gazetteer = Gazetteer::default();
        let geo = derive(&descriptions, &groups, &mut gazetteer, None, "now".into());

        assert_eq!(geo.stats.source_groups, 2);
        assert_eq!(geo.stats.sources, 1, "two readings of one title are one video");
        // The second fragment's frames inherit too — that only works because they share a source.
        assert_eq!(geo.images.len(), 7);
        assert_eq!(
            geo.coverage.get("Japan"),
            Some(&1),
            "one video must count once however many ways its title was read"
        );
    }

    #[test]
    fn fiction_titles_are_detected_from_the_group_title() {
        let r = resolver();
        assert!(r.is_fiction_title("Exploring Empty BF4 Maps"));
        assert!(r.is_fiction_title("Exploring Halo 3 Maps"));
        assert!(!r.is_fiction_title("Driving across the Pyrenees - YouTube"));
    }

    #[test]
    fn extracts_the_location_line() {
        assert_eq!(
            extract_location_line("Some prose.\nLocation: Jeju, South Korea"),
            Some("Jeju, South Korea".to_string())
        );
        assert_eq!(extract_location_line("No marker here"), None);
    }

    #[test]
    fn propagation_fills_unsampled_frames_and_skips_fiction() {
        let members: Vec<String> = (0..5).map(|i| format!("h{i}")).collect();
        let fiction: Vec<String> = (0..3).map(|i| format!("f{i}")).collect();
        let descriptions = vec![
            DescribedImage { hash: "h0".into(), description: "Location: Kyoto, Japan".into() },
            DescribedImage { hash: "f0".into(), description: "Location: China".into() },
        ];
        let groups = vec![
            SourceGroup { title: "Walking in Kyoto - YouTube", member_hashes: &members },
            SourceGroup { title: "Exploring Empty BF4 Maps", member_hashes: &fiction },
        ];
        let mut gazetteer = Gazetteer::default();
        let geo = derive(&descriptions, &groups, &mut gazetteer, None, "now".into());

        // All five frames of the real video are tagged from one described frame.
        assert_eq!(geo.images.len(), 5);
        assert!(geo.images.values().all(|r| r.countries == vec!["Japan".to_string()]));
        assert_eq!(geo.images["h0"].source, "own");
        assert_eq!(geo.images["h1"].source, "group");
        // The game video contributes nothing despite resolving to a real country.
        assert!(!geo.images.contains_key("f0"));
        assert_eq!(geo.stats.fiction_groups_skipped, 1);
        // One video is one source however many frames it has.
        assert_eq!(geo.coverage.get("Japan"), Some(&1));
    }

    #[test]
    fn sets_prefer_one_frame_per_video_and_badge_thin_countries() {
        // 20 videos, 3 frames each -> a fully diverse set.
        let mut images = BTreeMap::new();
        for video in 0..20 {
            for frame in 0..3 {
                images.insert(
                    format!("rich{video}_{frame}"),
                    GeoRecord {
                        countries: vec!["Japan".into()],
                        raw: "Japan".into(),
                        source: "group".into(),
                        via: None,
                        source_group: Some(video),
                        confidence: "high".into(),
                    },
                );
            }
        }
        // 2 videos, 30 frames each -> the Canada shape.
        for video in 100..102 {
            for frame in 0..30 {
                images.insert(
                    format!("thin{video}_{frame}"),
                    GeoRecord {
                        countries: vec!["Canada".into()],
                        raw: "Canada".into(),
                        source: "group".into(),
                        via: None,
                        source_group: Some(video),
                        confidence: "high".into(),
                    },
                );
            }
        }
        let geo = GeoFile {
            version: 1,
            generated_at: "now".into(),
            gazetteer_fingerprint: "0".into(),
            stats: GeoStats::default(),
            unresolved: BTreeMap::new(),
            coverage: source_counts(&images),
            previous_coverage: BTreeMap::new(),
            images,
        };
        let built = build_sets(&geo, 16, &BTreeMap::new(), &BTreeMap::new(), &[], "now".into());

        let japan = built.sets.iter().find(|s| s.country == "Japan").unwrap();
        assert_eq!(japan.size, 16);
        assert_eq!(japan.sources, 16, "one frame per video when the country can afford it");
        assert_eq!(japan.quality, "diverse");

        let canada = built.sets.iter().find(|s| s.country == "Canada").unwrap();
        assert_eq!(canada.size, 16, "thin countries still fill a set");
        assert_eq!(canada.sources, 2);
        assert_eq!(canada.quality, "limited", "but are never passed off as varied");

        // Rebuilding is stable — no reshuffling of a set already reviewed.
        let again = build_sets(&geo, 16, &BTreeMap::new(), &BTreeMap::new(), &[], "later".into());
        let japan_again = again.sets.iter().find(|s| s.country == "Japan").unwrap();
        assert_eq!(japan.members, japan_again.members);

        // A hand-excluded image must stay out of every rebuild — that is the whole point of the
        // exclusion file, and a rebuild silently reinstating one would make the action pointless.
        let mut excluded = BTreeMap::new();
        for hash in &japan.members {
            excluded.insert(hash.clone(), GeoExclusion::default());
        }
        let pruned = build_sets(&geo, 16, &excluded, &BTreeMap::new(), &[], "later".into());
        let japan_pruned = pruned.sets.iter().find(|s| s.country == "Japan").unwrap();
        assert!(
            japan_pruned.members.iter().all(|hash| !excluded.contains_key(hash)),
            "no excluded hash may reappear"
        );
        // Canada was untouched, so it must be unaffected by Japan's exclusions.
        assert!(pruned.sets.iter().any(|s| s.country == "Canada"));
    }

    #[test]
    fn scene_kinds_filter_set_membership_but_only_once_classified() {
        let mut images = BTreeMap::new();
        for video in 0..20 {
            images.insert(
                format!("h{video}"),
                GeoRecord {
                    countries: vec!["Japan".into()],
                    raw: "Japan".into(),
                    source: "group".into(),
                    via: None,
                    source_group: Some(video),
                    confidence: "high".into(),
                },
            );
        }
        let geo = GeoFile {
            version: 1,
            generated_at: "now".into(),
            gazetteer_fingerprint: "0".into(),
            stats: GeoStats::default(),
            unresolved: BTreeMap::new(),
            coverage: source_counts(&images),
            previous_coverage: BTreeMap::new(),
            images,
        };
        let allowed = vec!["outdoor".to_string()];

        // Nothing classified yet: the filter must not empty the set — an optional pass that has
        // not run should degrade to "no filtering", not to "no images".
        let unclassified = build_sets(&geo, 16, &BTreeMap::new(), &BTreeMap::new(), &allowed, "now".into());
        assert_eq!(unclassified.sets.iter().find(|s| s.country == "Japan").unwrap().size, 16);

        // Half indoor: only the outdoor half may be drawn on.
        let mut kinds = BTreeMap::new();
        for video in 0..20 {
            let kind = if video % 2 == 0 { "outdoor" } else { "indoor" };
            kinds.insert(format!("h{video}"), kind.to_string());
        }
        let filtered = build_sets(&geo, 16, &BTreeMap::new(), &kinds, &allowed, "now".into());
        let japan = filtered.sets.iter().find(|s| s.country == "Japan").unwrap();
        assert_eq!(japan.size, 10, "only the ten outdoor images remain");
        assert!(japan.members.iter().all(|hash| kinds[hash] == "outdoor"));
    }

    fn sets_of(members: &[&str]) -> GeoSetsFile {
        GeoSetsFile {
            version: GEO_SCHEMA_VERSION,
            generated_at: "2026-08-03T12:00:00.000Z".into(),
            target_size: 16,
            sets: vec![GeoSet {
                id: "country:japan:1".into(),
                kind: "country".into(),
                country: "Japan".into(),
                title: "Japan".into(),
                size: members.len(),
                sources: members.len(),
                max_per_source: 1,
                quality: "diverse".into(),
                members: members.iter().map(|hash| hash.to_string()).collect(),
                gazetteer_fingerprint: "0".into(),
                generated_at: "2026-08-03T12:00:00.000Z".into(),
            }],
        }
    }

    static EMPTY_KINDS: BTreeMap<String, String> = BTreeMap::new();

    /// The everything-agrees baseline: derived at 100, gazetteer written just before it, sets built
    /// after. Each test then overrides the one field it is about.
    fn status_input<'a>(
        sets: &'a GeoSetsFile,
        excluded: &'a BTreeMap<String, GeoExclusion>,
    ) -> StatusInput<'a> {
        StatusInput {
            sets: Some(sets),
            excluded,
            kinds: &EMPTY_KINDS,
            allowed_kinds: &[],
            derived_at: Some(100),
            gazetteer_at: Some(90),
            sets_at: Some(110),
        }
    }

    #[test]
    fn status_is_quiet_when_every_sidecar_agrees() {
        let sets = sets_of(&["a", "b"]);
        let excluded = BTreeMap::new();
        let status = status(&status_input(&sets, &excluded));
        assert!(!status.records_stale && !status.sets_stale);
        assert!(status.reasons.is_empty(), "{:?}", status.reasons);
        assert_eq!(status.set_members, 2);
        assert_eq!(status.sets_built_at.as_deref(), Some("2026-08-03T12:00:00.000Z"));
    }

    #[test]
    fn status_reports_an_exclusion_the_sets_predate() {
        // The super-image-viewer case: another app wrote the exclusion while this window sat on a
        // set list built before it. Nothing about the timestamps says so — only the intersection.
        let sets = sets_of(&["a", "b"]);
        let mut excluded = BTreeMap::new();
        excluded.insert("b".to_string(), GeoExclusion::default());
        let status = status(&status_input(&sets, &excluded));
        assert_eq!(status.excluded_in_sets, 1);
        assert!(status.sets_stale);
        assert_eq!(status.reasons.len(), 1);
        assert!(status.reasons[0].contains("1 hand-excluded image is"), "{:?}", status.reasons);
    }

    #[test]
    fn status_reports_a_member_since_classified_out_of_bounds() {
        let sets = sets_of(&["a", "b"]);
        let excluded = BTreeMap::new();
        let kinds: BTreeMap<String, String> =
            [("b".to_string(), "person".to_string())].into_iter().collect();
        let allowed = vec!["outdoor".to_string()];
        let status = status(&StatusInput {
            kinds: &kinds,
            allowed_kinds: &allowed,
            ..status_input(&sets, &excluded)
        });
        assert_eq!(status.disallowed_in_sets, 1);
        assert!(status.sets_stale);
        // An unclassified member is not drift: `build_sets` lets it through, so the report must too.
        assert_eq!(status.excluded_in_sets, 0);
    }

    #[test]
    fn status_reads_write_order_for_the_two_it_cannot_intersect() {
        let sets = sets_of(&["a"]);
        let excluded = BTreeMap::new();

        // Gazetteer edited after the derive: the records have not been through those decisions.
        let edited = status(&StatusInput {
            gazetteer_at: Some(120),
            ..status_input(&sets, &excluded)
        });
        assert!(edited.gazetteer_newer_than_records && edited.records_stale);

        // Re-derived after the sets were built: the sets are the older half of the screen.
        let rederived = status(&StatusInput {
            derived_at: Some(200),
            ..status_input(&sets, &excluded)
        });
        assert!(rederived.records_newer_than_sets && rederived.sets_stale);

        // A derive writes the gazetteer immediately before the records, so equal stamps — the
        // common case on a fast machine — must not accuse the derive of leaving work undone.
        let same_second = status(&StatusInput {
            gazetteer_at: Some(100),
            ..status_input(&sets, &excluded)
        });
        assert!(!same_second.gazetteer_newer_than_records);
    }

    #[test]
    fn status_says_nothing_before_the_first_derive() {
        let excluded = BTreeMap::new();
        let kinds = BTreeMap::new();
        let status = status(&StatusInput {
            sets: None,
            excluded: &excluded,
            kinds: &kinds,
            allowed_kinds: &[],
            derived_at: None,
            gazetteer_at: Some(90),
            sets_at: None,
        });
        assert!(!status.has_records && !status.has_sets);
        assert!(status.reasons.is_empty(), "an empty library is not out of date");
    }
}
