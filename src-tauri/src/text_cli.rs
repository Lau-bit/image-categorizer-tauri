//! `icat` — the agent-facing door to the extracted-text layer.
//!
//! This exists because the GUI cannot be the interface. Synthetic input to a WebView2 window can
//! only be delivered to the foreground, so any agent driving the UI steals the focus of whoever is
//! actually using the machine. A CLI over the same index the panel reads costs one process, takes
//! no focus, and — the part that matters most — **works with the app closed**.
//!
//! What it may and may not do, as settled with the user:
//!
//! - It builds the index itself when one is missing or stale. That is a derived artifact.
//! - It never runs OCR extraction implicitly. `icat extract` is the explicit update run, because an
//!   accidental one is thousands of OCR calls nobody asked for.
//! - It writes sidecars (categories, folder scope) under the same [`SidecarLock`] the app takes, so
//!   a concurrent GUI or a second agent cannot interleave a read-modify-write.
//! - It never touches an original image. Nothing here opens an image file for writing.
//!
//! Every text-bearing output is redacted (see [`crate::redact`]) unless `--raw` is passed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::redact;
use crate::text_index::{self, QueryOptions, TextIndex};
use crate::{
    apply_idle_ttl, build_text_index_for, commit_extraction_results, indexed_categories,
    load_library_config, now_iso, pending_text_extraction, save_library_config, scan_and_reconcile,
    text_dir_path, text_index_staleness, vision_api_key, vision_endpoint, vision_model, topics,
    vision, AppSettings, SidecarLock,
};

const USAGE: &str = r#"icat — search the text Image Categorizer extracted from your screenshots

USAGE
  icat <command> [options]

COMMANDS
  status                      index vintage, coverage, folder scope
  search <query>              rank documents; quoted "phrases" supported
  timeline                    images and keywords per time bucket
  show <hash|path>            one document's full text
  categorize --to <category>  assign a category to hits or hashes
  extract                     run OCR extraction over images that have none
  topics                      name what each time bucket was about (local LLM)
  index                       build or rebuild the search index
  folders                     list source folders / change what is indexed

COMMON OPTIONS
  --root <path>       library root (default: $ICAT_ROOT, else the app's last root)
  --json              machine-readable output
  --raw               skip credential redaction (local reading only)
  --no-build          fail instead of building a missing or stale index

SEARCH / TIMELINE OPTIONS
  --from <when>       2026-08-03 | 2026-08-03T14:00 | 7d | today | yesterday
  --to <when>         a bare date means end of that day
  --budget <chars>    cap total output text (default 6000; 0 = unlimited)
  -k, --limit <n>     cap the number of hits (default 20; 0 = unlimited)
  --snippet <chars>   per-hit snippet width (default 300)
  --bucket <hours>    timeline bucket width (default 48)
  --folder <name>     restrict to a source folder (repeatable)
  --category <name>   restrict to a category (repeatable)
  --min-chars <n>     ignore documents shorter than this
  --all               require every query term rather than scoring a union
  --dupes             include exact duplicates, which are collapsed by default
  --full              print whole documents instead of snippets

TOPICS OPTIONS
  --bucket <hours>    width to name buckets at (default 48; stored per width)
  --from / --to       which buckets to name; a bucket only half in range is still
                      named as the whole bucket it is
  --force             re-run buckets that already have topics
  --limit <n>         stop after n buckets
  --vocabulary        print the topic/name vocabulary instead of generating

CATEGORIZE OPTIONS
  --to <category>     required; must already exist, so a typo cannot fork the taxonomy
  --hash <h>          repeatable, or comma-separated
  --query <q>         categorize everything the query matches — ALL of it, not a page.
                      Pass -k <n> to cap it deliberately.
  --from / --until    bound the query by time. The upper bound is `--until` here,
                      because `--to` already names the category.

EXAMPLES
  icat search "webview2 cdp" --from 7d
  icat search "\"os error 5\"" --budget 3000 --json
  icat timeline --from 2026-08-01 --bucket 24
  icat show a3f9c2b1d4e5f607 --group
  icat topics --bucket 168
  icat topics --vocabulary
"#;

/// Entry point for the `icat` binary. Returns the process exit code.
pub fn run() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || argv[0] == "--help" || argv[0] == "-h" || argv[0] == "help" {
        print!("{USAGE}");
        return 0;
    }

    let args = match Args::parse(&argv) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("icat: {error}");
            return 2;
        }
    };

    let result = match args.command.as_str() {
        "status" => cmd_status(&args),
        "search" => cmd_search(&args),
        "timeline" => cmd_timeline(&args),
        "show" => cmd_show(&args),
        "categorize" => cmd_categorize(&args),
        "extract" => cmd_extract(&args),
        "topics" => cmd_topics(&args),
        "index" => cmd_index(&args),
        "folders" => cmd_folders(&args),
        other => Err(format!("unknown command `{other}`. Run `icat --help`.")),
    };

    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("icat: {error}");
            1
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------------------------

/// Flags that are switches, never value-takers. Listed explicitly because the alternative — "a flag
/// swallows the next token unless that token starts with `--`" — cannot tell a switch from a value
/// flag when a bare word follows, and silently eats it: `icat search --full tauri webview2` searched
/// for `webview2` alone and dropped `tauri` without a word, returning a confident wrong answer.
/// Anything not named here keeps the old heuristic, so an unknown flag still behaves sensibly.
const SWITCHES: &[&str] = &[
    "json", "raw", "no-build", "all", "dupes", "full", "force", "group", "hashes", "vocabulary",
    "all-categories", "help",
];

struct Args {
    command: String,
    positional: Vec<String>,
    flags: HashMap<String, Vec<String>>,
}

impl Args {
    fn parse(argv: &[String]) -> Result<Self, String> {
        let mut args = Args {
            command: argv[0].clone(),
            positional: Vec::new(),
            flags: HashMap::new(),
        };
        let mut index = 1usize;
        while index < argv.len() {
            let token = &argv[index];
            if let Some(rest) = token.strip_prefix("--") {
                let (name, inline) = match rest.split_once('=') {
                    Some((name, value)) => (name.to_string(), Some(value.to_string())),
                    None => (rest.to_string(), None),
                };
                let value = match inline {
                    // `--json=false` is still honoured for anything that reads the value back.
                    Some(value) => value,
                    // A known switch never consumes what follows it, so a query word after one
                    // stays part of the query.
                    None if SWITCHES.contains(&name.as_str()) => "true".to_string(),
                    None => {
                        // Anything else takes the next token as its value unless that token is
                        // itself a flag.
                        match argv.get(index + 1) {
                            Some(next) if !next.starts_with("--") && !is_short_flag(next) => {
                                index += 1;
                                next.clone()
                            }
                            _ => "true".to_string(),
                        }
                    }
                };
                args.flags.entry(name).or_default().push(value);
            } else if token == "-k" {
                let value = argv
                    .get(index + 1)
                    .ok_or_else(|| "-k needs a number".to_string())?;
                index += 1;
                args.flags.entry("limit".to_string()).or_default().push(value.clone());
            } else {
                args.positional.push(token.clone());
            }
            index += 1;
        }
        Ok(args)
    }

    fn flag(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|values| values.last()).map(String::as_str)
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.flags.get(name).cloned().unwrap_or_default()
    }

    fn is_set(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    fn number(&self, name: &str, default: usize) -> Result<usize, String> {
        match self.flag(name) {
            Some(value) => value
                .parse()
                .map_err(|_| format!("--{name} expects a number, got `{value}`")),
            None => Ok(default),
        }
    }

    fn json(&self) -> bool {
        self.is_set("json")
    }

    fn raw(&self) -> bool {
        self.is_set("raw")
    }
}

fn is_short_flag(token: &str) -> bool {
    token == "-k"
}

// ---------------------------------------------------------------------------------------------
// Root and settings
// ---------------------------------------------------------------------------------------------

/// Where the app keeps its settings when it is not running to tell us. Mirrors what Tauri's
/// `app_data_dir()` resolves to for this identifier, so the CLI and the GUI agree on "last root"
/// without the GUI having to be up.
fn app_settings_path() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(
        PathBuf::from(appdata)
            .join("com.slaur.image-categorizer")
            .join("settings.json"),
    )
}

fn resolve_root(args: &Args) -> Result<PathBuf, String> {
    let candidate = args
        .flag("root")
        .map(str::to_string)
        .or_else(|| std::env::var("ICAT_ROOT").ok())
        .or_else(|| {
            let path = app_settings_path()?;
            let settings: AppSettings = fs::read_to_string(path)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            settings.last_root
        })
        .ok_or_else(|| {
            "no library root. Pass --root <path>, set ICAT_ROOT, or open a root in the app once."
                .to_string()
        })?;

    let path = PathBuf::from(&candidate);
    if !path.is_dir() {
        return Err(format!("library root does not exist: {candidate}"));
    }
    Ok(path)
}

// ---------------------------------------------------------------------------------------------
// Index building and staleness
// ---------------------------------------------------------------------------------------------

/// Loads the index, building it when it is missing or stale. Building is allowed without asking —
/// it is derived from files already on disk. Running OCR is not, and never happens here.
fn load_or_build(root: &Path, args: &Args) -> Result<TextIndex, String> {
    let config = load_library_config(root);
    let existing = text_index::load(root);

    let reason = match &existing {
        None => Some("no index yet".to_string()),
        Some(index) => text_index_staleness(root, index),
    };

    let Some(reason) = reason else {
        return Ok(existing.expect("checked above"));
    };

    if args.is_set("no-build") {
        return match existing {
            Some(index) => {
                eprintln!("icat: index is stale ({reason}); --no-build, using it as is");
                Ok(index)
            }
            None => Err(format!("no index and --no-build was passed ({reason})")),
        };
    }

    eprintln!("icat: building index ({reason})…");
    let index = build_text_index_for(root, &config);
    text_index::save(root, &index)?;
    eprintln!(
        "icat: indexed {} documents, {} terms, {} ms",
        index.report.docs, index.report.terms, index.report.elapsed_ms
    );
    Ok(index)
}

// ---------------------------------------------------------------------------------------------
// Time parsing
// ---------------------------------------------------------------------------------------------

/// Local wall-clock now, as the same naive-local seconds the index stores. Read from the OS rather
/// than derived from a UTC clock because there is no offset stored anywhere else in this app.
fn local_now() -> i64 {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let time = unsafe { GetLocalTime() };
    text_index::civil_to_secs(
        time.wYear as i64,
        time.wMonth as u32,
        time.wDay as u32,
        time.wHour as u32,
        time.wMinute as u32,
        time.wSecond as u32,
    )
}

/// Parses `--from` / `--to`. `end_of_day` makes a bare date inclusive on the `--to` side, so
/// `--from 2026-08-03 --to 2026-08-03` is that whole day rather than its first instant.
fn parse_when(value: &str, end_of_day: bool) -> Result<i64, String> {
    let value = value.trim();
    let now = local_now();

    match value {
        "now" => return Ok(now),
        "today" => {
            let midnight = now.div_euclid(86_400) * 86_400;
            return Ok(if end_of_day { midnight + 86_399 } else { midnight });
        }
        "yesterday" => {
            let midnight = now.div_euclid(86_400) * 86_400 - 86_400;
            return Ok(if end_of_day { midnight + 86_399 } else { midnight });
        }
        _ => {}
    }

    // Relative windows: `7d`, `36h`, `90m` all mean "that long ago".
    if let Some(rest) = value.strip_suffix(|c| c == 'd' || c == 'h' || c == 'm') {
        if let Ok(count) = rest.parse::<i64>() {
            let unit = value.chars().last().unwrap_or('d');
            let seconds = match unit {
                'd' => 86_400,
                'h' => 3_600,
                _ => 60,
            };
            return Ok(now - count * seconds);
        }
    }

    let (date_part, time_part) = match value.split_once(['T', ' ']) {
        Some((date, time)) => (date, Some(time)),
        None => (value, None),
    };
    let fields: Vec<&str> = date_part.split('-').collect();
    if fields.len() != 3 {
        return Err(format!(
            "cannot read `{value}` as a time. Use 2026-08-03, 2026-08-03T14:00, 7d, today or yesterday."
        ));
    }
    let year: i64 = fields[0].parse().map_err(|_| format!("bad year in `{value}`"))?;
    let month: u32 = fields[1].parse().map_err(|_| format!("bad month in `{value}`"))?;
    let day: u32 = fields[2].parse().map_err(|_| format!("bad day in `{value}`"))?;

    let (hour, minute, second) = match time_part {
        Some(time) => {
            let parts: Vec<&str> = time.split(':').collect();
            let hour: u32 = parts.first().unwrap_or(&"0").parse().unwrap_or(0);
            let minute: u32 = parts.get(1).map(|v| v.parse().unwrap_or(0)).unwrap_or(0);
            let second: u32 = parts.get(2).map(|v| v.parse().unwrap_or(0)).unwrap_or(0);
            (hour, minute, second)
        }
        None if end_of_day => (23, 59, 59),
        None => (0, 0, 0),
    };

    Ok(text_index::civil_to_secs(year, month, day, hour, minute, second))
}

fn query_options(args: &Args, query: String) -> Result<QueryOptions, String> {
    query_options_for(args, query, "to")
}

/// `upper_bound_flag` exists because **`--to` means two different things**. In `search` and
/// `timeline` it is the end of the time range; in `categorize` it is the destination category. They
/// collided: `categorize --query tauri --to Archive` fed `Archive` to the time parser and died with
/// *"cannot read `Archive` as a time"*, so the `--query` form of categorize could never run at all.
/// Categorize therefore bounds its upper end with `--until`, and `--to` stays the category there.
fn query_options_for(args: &Args, query: String, upper_bound_flag: &str) -> Result<QueryOptions, String> {
    Ok(QueryOptions {
        query,
        from: match args.flag("from") {
            Some(value) => Some(parse_when(value, false)?),
            None => None,
        },
        to: match args.flag(upper_bound_flag) {
            Some(value) => Some(parse_when(value, true)?),
            None => None,
        },
        folders: args.all("folder"),
        categories: args.all("category"),
        min_chars: args.number("min-chars", 0)? as u32,
        include_dupes: args.is_set("dupes"),
        require_all: args.is_set("all"),
        limit: args.number("limit", 20)?,
    })
}

/// How many hits a **write** may act on. Reads page — 20 is a sensible screenful of search results
/// — but a write inheriting that default silently filed the top 20 of a 1,471-match query and then
/// reported "20" as if that had been the whole match set. A bulk edit takes everything it matched
/// unless the caller caps it on purpose.
fn write_limit(args: &Args) -> Result<usize, String> {
    match args.flag("limit") {
        Some(_) => args.number("limit", 0),
        None => Ok(0),
    }
}

fn maybe_redact(text: &str, raw: bool) -> String {
    if raw {
        text.to_string()
    } else {
        redact::redact_text(text)
    }
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

fn cmd_status(args: &Args) -> Result<(), String> {
    let root = resolve_root(args)?;
    let config = load_library_config(&root);
    let categories = indexed_categories(&config);

    // Coverage is counted off the records, which is exact and needs no scan.
    let mut in_scope = 0usize;
    let mut extracted = 0usize;
    for record in config.images.values() {
        let category = record.category.clone().unwrap_or_default();
        if !categories.contains(&category) {
            continue;
        }
        in_scope += 1;
        if record.ocr_text_chars.is_some() {
            extracted += 1;
        }
    }

    let index = text_index::load(&root);
    let stale = index.as_ref().and_then(|index| text_index_staleness(&root, index));

    if args.json() {
        let payload = serde_json::json!({
            "root": root.to_string_lossy(),
            "categories": categories,
            "inScope": in_scope,
            "extracted": extracted,
            "pendingExtraction": in_scope.saturating_sub(extracted),
            "index": index.as_ref().map(|index| serde_json::json!({
                "builtAt": index.built_at,
                "docs": index.report.docs,
                "terms": index.report.terms,
                "groups": index.report.groups,
                "exactDupes": index.report.exact_dupes,
                "nearDupes": index.report.near_dupes,
                "totalChars": index.total_chars,
                "span": index.span().map(|(first, last)| serde_json::json!({
                    "from": text_index::format_date(first),
                    "to": text_index::format_date(last),
                })),
            })),
            "stale": stale,
            "excludedFolders": config.text_index_excluded_folders,
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return Ok(());
    }

    println!("Root      {}", root.to_string_lossy());
    println!("Scope     {} ({in_scope} images)", categories.join(", "));
    match &index {
        Some(index) => {
            println!(
                "Index     {} docs · {} terms · {} chars · built {}",
                index.report.docs,
                index.report.terms,
                human_count(index.total_chars),
                index.built_at
            );
            println!(
                "Groups    {} ({} exact hidden, {} near demoted)",
                index.report.groups, index.report.exact_dupes, index.report.near_dupes
            );
            if let Some((first, last)) = index.span() {
                println!(
                    "Span      {} .. {}",
                    text_index::format_date(first),
                    text_index::format_date(last)
                );
            }
            match &stale {
                Some(reason) => println!("Vintage   stale — {reason} (any query rebuilds it)"),
                None => println!("Vintage   current"),
            }
        }
        None => println!("Index     none yet — any query builds one"),
    }
    let pending = in_scope.saturating_sub(extracted);
    println!(
        "Coverage  {extracted}/{in_scope} extracted{}",
        if pending == 0 {
            String::new()
        } else {
            format!(" · {pending} pending — `icat extract` when you want them")
        }
    );
    if !config.text_index_excluded_folders.is_empty() {
        println!("Excluded  {}", config.text_index_excluded_folders.join(", "));
    }
    Ok(())
}

fn cmd_index(args: &Args) -> Result<(), String> {
    let root = resolve_root(args)?;
    let config = load_library_config(&root);
    let index = build_text_index_for(&root, &config);
    text_index::save(&root, &index)?;

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&index.report).unwrap_or_default()
        );
    } else {
        println!(
            "Indexed {} documents · {} terms · {} groups ({} exact, {} near) · {} ms",
            index.report.docs,
            index.report.terms,
            index.report.groups,
            index.report.exact_dupes,
            index.report.near_dupes,
            index.report.elapsed_ms
        );
        if index.report.skipped_missing > 0 {
            println!(
                "{} in-scope images had no extracted text yet — `icat extract` to add them",
                index.report.skipped_missing
            );
        }
        // Separate line, because `icat extract` cannot move this number: OCR ran and found no text.
        if index.report.skipped_empty > 0 {
            println!(
                "{} had text extracted but it was empty — nothing to index, and re-running \
                 extraction will not change that",
                index.report.skipped_empty
            );
        }
    }
    Ok(())
}

fn cmd_search(args: &Args) -> Result<(), String> {
    let query = args.positional.join(" ");
    if query.trim().is_empty() {
        return Err("search needs a query. `icat search \"webview2 cdp\"`".to_string());
    }
    let root = resolve_root(args)?;
    let index = load_or_build(&root, args)?;
    let options = query_options(args, query.clone())?;
    let texts = text_dir_path(&root);
    let outcome = text_index::search(&index, &options, Some(&texts));

    let budget = args.number("budget", 6000)?;
    let snippet_width = args.number("snippet", 300)?;
    let full = args.is_set("full");
    let raw = args.raw();

    let mut used = 0usize;
    let mut shown = 0usize;
    let mut rendered: Vec<(usize, String)> = Vec::new();

    for (position, hit) in outcome.hits.iter().enumerate() {
        let text = fs::read_to_string(text_index::text_file_path(&texts, &hit.hash)).unwrap_or_default();
        let body = if full {
            text.clone()
        } else {
            text_index::snippet(&text, &collect_terms(&options.query), snippet_width)
        };
        let body = maybe_redact(&body, raw);
        if budget > 0 && used + body.chars().count() > budget && shown > 0 {
            break;
        }
        used += body.chars().count();
        shown += 1;
        rendered.push((position, body));
    }

    let dropped = outcome.hits.len() - shown;

    if args.json() {
        let hits: Vec<serde_json::Value> = rendered
            .iter()
            .map(|(position, body)| {
                let hit = &outcome.hits[*position];
                serde_json::json!({
                    "hash": hit.hash,
                    "path": hit.path,
                    "at": text_index::format_datetime(hit.ts),
                    "score": hit.score,
                    "chars": hit.chars,
                    "rank": rank_name(hit.rank),
                    "keywords": hit.terms,
                    "exactDupes": hit.exact_dupes,
                    "nearDupes": hit.near_dupes,
                    "text": body,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "query": query,
            "matched": outcome.matched,
            "shown": shown,
            "droppedForBudget": dropped,
            "budget": budget,
            "charsUsed": used,
            "unknownTerms": outcome.unknown_terms,
            "phraseChecked": outcome.phrase_checked,
            "phraseCapped": outcome.phrase_capped,
            "redacted": !raw,
            "hits": hits,
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return Ok(());
    }

    println!(
        "{} matched · {shown} shown{} · {used} chars{}",
        outcome.matched,
        if dropped > 0 {
            format!(" · {dropped} held back by --budget {budget}")
        } else {
            String::new()
        },
        if raw { " · RAW" } else { "" }
    );
    if outcome.no_searchable_terms {
        println!(
            "nothing was searched for: every word in that query is a stopword or shorter than {} \
             characters, so none of them are in the index. This is not the same as finding nothing.",
            text_index::MIN_TOKEN_LEN
        );
    }
    if !outcome.unknown_terms.is_empty() {
        println!("never on screen: {}", outcome.unknown_terms.join(", "));
    }
    if outcome.phrase_capped {
        println!(
            "phrase verified against the top {} candidates only",
            outcome.phrase_checked
        );
    }
    println!();

    for (position, body) in rendered {
        let hit = &outcome.hits[position];
        let mut badges = Vec::new();
        if hit.rank == text_index::RANK_NEAR_DUPE {
            badges.push("near-dupe".to_string());
        }
        if hit.exact_dupes > 0 {
            badges.push(format!("+{} identical", hit.exact_dupes));
        }
        if hit.near_dupes > 0 {
            badges.push(format!("+{} near", hit.near_dupes));
        }
        println!(
            "[{}] {} · {} · {:.1}{}",
            position + 1,
            text_index::format_datetime(hit.ts),
            hit.hash,
            hit.score,
            if badges.is_empty() {
                String::new()
            } else {
                format!(" · {}", badges.join(", "))
            }
        );
        println!("     {}", hit.path);
        if !hit.terms.is_empty() {
            println!("     kw: {}", hit.terms.join(", "));
        }
        println!("     {body}");
        println!();
    }

    Ok(())
}

fn collect_terms(query: &str) -> Vec<String> {
    text_index::split_query(query).0
}

fn rank_name(rank: u8) -> &'static str {
    match rank {
        text_index::RANK_EXACT_DUPE => "exact-dupe",
        text_index::RANK_NEAR_DUPE => "near-dupe",
        _ => "representative",
    }
}

fn cmd_timeline(args: &Args) -> Result<(), String> {
    let root = resolve_root(args)?;
    let index = load_or_build(&root, args)?;
    let options = query_options(args, String::new())?;
    let width = args.number("bucket", text_index::DEFAULT_BUCKET_HOURS as usize)? as u32;
    let topics_file = topics::load(&root);
    let mut buckets = topics::timeline_with_topics(&index, &options, width, &topics_file);
    if !args.is_set("hashes") {
        for bucket in buckets.iter_mut() {
            bucket.hashes.clear();
        }
    }

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "bucketHours": width,
                "buckets": buckets,
            }))
            .unwrap_or_default()
        );
        return Ok(());
    }

    if buckets.is_empty() {
        println!("No documents in range.");
        return Ok(());
    }

    let named = buckets.iter().filter(|bucket| !bucket.topics.is_empty()).count();
    println!(
        "{} buckets at {}h{}\n",
        buckets.len(),
        width,
        if named > 0 {
            format!(" · {named} named")
        } else {
            String::new()
        }
    );
    for bucket in &buckets {
        println!(
            "{:<22} {:>5} img{} ({} rep, {} exact, {} near) · {} chars",
            bucket.id,
            bucket.images,
            // Said plainly, because the topics printed underneath describe the WHOLE bucket while
            // these counts describe only the part your range let through.
            if bucket.partial {
                format!(" of {}", bucket.members)
            } else {
                String::new()
            },
            bucket.representatives,
            bucket.exact_dupes,
            bucket.near_dupes,
            human_count(bucket.chars)
        );
        // Topics first when they exist: they are the answer to "what was this about", and the
        // statistical terms are the raw material they were made from. Both are printed, because the
        // terms carry exact spellings (identifiers, error strings) that a topic phrase smooths away.
        if !bucket.topics.is_empty() {
            println!(
                "{:<22} {}{}",
                "",
                bucket.topics.join(" · "),
                if bucket.topics_stale { "   (stale — bucket changed)" } else { "" }
            );
        }
        if !bucket.notable.is_empty() {
            println!("{:<22} {}", "", bucket.notable.join(", "));
        }
        if !bucket.terms.is_empty() {
            println!("{:<22} kw: {}", "", bucket.terms.join(", "));
        }
    }
    Ok(())
}

/// Names each time bucket with the local model. One call per bucket — see `topics.rs` for why this
/// is per bucket and not per image.
fn cmd_topics(args: &Args) -> Result<(), String> {
    let root = resolve_root(args)?;
    let width = args.number("bucket", text_index::DEFAULT_BUCKET_HOURS as usize)? as u32;
    let mut file = topics::load(&root);

    if args.is_set("vocabulary") {
        let (topic_list, notable) = topics::vocabulary(&file, width);
        if args.json() {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "bucketHours": width,
                    "topics": topic_list,
                    "notable": notable,
                }))
                .unwrap_or_default()
            );
            return Ok(());
        }
        if topic_list.is_empty() {
            println!("No topics at {width}h yet. Run `icat topics --bucket {width}`.");
            return Ok(());
        }
        println!("Topics ({} distinct, by buckets using them)", topic_list.len());
        for (name, count) in topic_list.iter().take(40) {
            println!("  {count:>3}  {name}");
        }
        println!("\nNamed things ({} distinct)", notable.len());
        for (name, count) in notable.iter().take(40) {
            println!("  {count:>3}  {name}");
        }
        return Ok(());
    }

    let index = load_or_build(&root, args)?;
    if index.docs.is_empty() {
        return Err("nothing is indexed yet".to_string());
    }
    let texts = text_dir_path(&root);
    // `--from`/`--to` choose WHICH buckets get named; they never narrow what a bucket is about.
    // `timeline` fills membership from the whole corpus, so a bucket only half inside the range is
    // still described — and fingerprinted — as the whole bucket it is. Ignoring these flags here
    // meant `icat topics --from 7d` quietly queued every bucket in the library, at a model call
    // and ~12s each.
    let options = query_options(args, String::new())?;
    let buckets = text_index::timeline(&index, &options, width, true);
    let mut pending: Vec<text_index::Bucket> = topics::pending(&buckets, &file, width, args.is_set("force"))
        .into_iter()
        .cloned()
        .collect();

    let limit = args.number("limit", 0)?;
    let total_pending = pending.len();
    if limit > 0 {
        pending.truncate(limit);
    }

    if pending.is_empty() {
        println!("Every bucket at {width}h already has topics.");
        return Ok(());
    }

    // Settings are read the same way the GUI reads them, so the CLI cannot end up talking to a
    // different endpoint or model than the panel does.
    let settings = load_cli_settings();
    let endpoint = vision_endpoint(&settings);
    let model = vision_model(&settings);
    let api_key = vision_api_key(&settings);
    // Part one of the idle lease — the `ttl` stamped on every request, which only ever shortens a
    // load THIS process caused. The ownership tracking and keep-alive halves are GUI concerns (they
    // need an app handle and a live window); a CLI run is short-lived and wants exactly this.
    apply_idle_ttl(&settings);

    println!(
        "Naming {} of {total_pending} buckets at {width}h with {model}…",
        pending.len()
    );

    let agent = vision::build_agent();
    let total = pending.len();
    let mut failures = 0usize;

    for (position, bucket) in pending.iter().enumerate() {
        let (prompt, sampled) = topics::prepare(
            bucket,
            |hash| fs::read_to_string(text_index::text_file_path(&texts, hash)).ok(),
            |hash| index.doc_by_hash(hash).map(|(_, doc)| doc.terms.clone()).unwrap_or_default(),
            |hash| {
                index
                    .doc_by_hash(hash)
                    .map(|(_, doc)| text_index::format_datetime(doc.ts))
                    .unwrap_or_default()
            },
            topics::DEFAULT_PROMPT_BUDGET,
        );
        if sampled == 0 {
            failures += 1;
            eprintln!("  [{}/{total}] {} — no readable text", position + 1, bucket.id);
            continue;
        }

        match topics::ask(&agent, &endpoint, &model, api_key.as_deref(), &prompt) {
            Ok((topic_list, notable)) => {
                println!(
                    "  [{}/{total}] {}  {}",
                    position + 1,
                    bucket.id,
                    topic_list.join(" · ")
                );
                topics::record(
                    &mut file, width, bucket, topic_list, notable, sampled, &model, &now_iso(),
                );
                // Per bucket, not at the end: each one cost a model call, and a run interrupted at
                // forty should keep forty.
                topics::save(&root, &file)?;
            }
            Err(error) => {
                failures += 1;
                eprintln!("  [{}/{total}] {} — {error}", position + 1, bucket.id);
            }
        }
    }

    println!(
        "Named {} of {total}{}.",
        total - failures,
        if failures > 0 { format!(", {failures} failed") } else { String::new() }
    );
    if limit > 0 && total_pending > total {
        println!("{} buckets still pending — run again to continue.", total_pending - total);
    }
    Ok(())
}

/// The app's settings, read from disk. The GUI gets these through Tauri's app handle; with the
/// window closed there is no handle, and this is the same file it would have read.
fn load_cli_settings() -> AppSettings {
    app_settings_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn cmd_show(args: &Args) -> Result<(), String> {
    let target = args
        .positional
        .first()
        .ok_or_else(|| "show needs a hash or a path".to_string())?;
    let root = resolve_root(args)?;
    let index = load_or_build(&root, args)?;
    let texts = text_dir_path(&root);

    let (doc_index, doc) = match index.doc_by_hash(target) {
        Some(found) => found,
        None => {
            let needle = target.replace('\\', "/").to_lowercase();
            index
                .docs
                .iter()
                .enumerate()
                .find(|(_, doc)| {
                    doc.path.to_lowercase().ends_with(&needle) || doc.name.to_lowercase() == needle
                })
                .ok_or_else(|| format!("no indexed document matches `{target}`"))?
        }
    };

    let raw = args.raw();
    let text = fs::read_to_string(text_index::text_file_path(&texts, &doc.hash))
        .map_err(|error| format!("no extracted text for {}: {error}", doc.hash))?;
    let body = maybe_redact(&text, raw);

    let members = index.group_members(doc_index);
    let mut deltas: Vec<(String, Vec<String>)> = Vec::new();
    if args.is_set("group") {
        let representative =
            fs::read_to_string(text_index::text_file_path(&texts, &index.docs[members[0]].hash))
                .unwrap_or_default();
        for member in members.iter().skip(1) {
            let member_doc = &index.docs[*member];
            let member_text =
                fs::read_to_string(text_index::text_file_path(&texts, &member_doc.hash)).unwrap_or_default();
            let novel = text_index::novel_lines(&representative, &member_text);
            if !novel.is_empty() {
                deltas.push((
                    member_doc.hash.clone(),
                    novel.into_iter().map(|line| maybe_redact(&line, raw)).collect(),
                ));
            }
        }
    }

    if args.json() {
        let payload = serde_json::json!({
            "hash": doc.hash,
            "path": doc.path,
            "at": text_index::format_datetime(doc.ts),
            "category": doc.category,
            "folder": doc.folder,
            "chars": doc.chars,
            "rank": rank_name(doc.rank),
            "keywords": doc.terms,
            "groupSize": members.len(),
            "redacted": !raw,
            "text": body,
            "novelLines": deltas.iter().map(|(hash, lines)| serde_json::json!({
                "hash": hash,
                "lines": lines,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        return Ok(());
    }

    println!("{}  {}", doc.hash, text_index::format_datetime(doc.ts));
    println!("{}", doc.path);
    println!(
        "{} · {} chars · {}{}",
        doc.category,
        doc.chars,
        rank_name(doc.rank),
        if members.len() > 1 {
            format!(" · group of {}", members.len())
        } else {
            String::new()
        }
    );
    if !doc.terms.is_empty() {
        println!("kw: {}", doc.terms.join(", "));
    }
    println!("{}", "-".repeat(72));
    println!("{body}");

    if args.is_set("group") {
        if deltas.is_empty() && members.len() > 1 {
            println!("\n(no group member added any line this one does not have)");
        }
        for (hash, lines) in deltas {
            println!("\n+ {hash} added:");
            for line in lines {
                println!("    {line}");
            }
        }
    }
    Ok(())
}

fn cmd_categorize(args: &Args) -> Result<(), String> {
    let category = args
        .flag("to")
        .ok_or_else(|| "categorize needs --to <category>".to_string())?
        .to_string();
    let root = resolve_root(args)?;

    let mut hashes: Vec<String> = args
        .all("hash")
        .iter()
        .flat_map(|value| value.split(',').map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
        .collect();

    let mut matched = 0usize;
    if let Some(query) = args.flag("query") {
        let index = load_or_build(&root, args)?;
        let mut options = query_options_for(args, query.to_string(), "until")?;
        options.limit = write_limit(args)?;
        let outcome = text_index::search(&index, &options, Some(&text_dir_path(&root)));
        matched = outcome.matched;
        hashes.extend(outcome.hits.iter().map(|hit| hit.hash.clone()));
    }

    hashes.sort();
    hashes.dedup();
    if hashes.is_empty() {
        return Err("nothing to categorize. Pass --hash <h> or --query <q>.".to_string());
    }

    // Cross-process: the GUI and image-viewer-tauri write this same sidecar. Held across the whole
    // read-modify-write, exactly as `assign_category` does.
    let _lock = SidecarLock::acquire(&root);
    let mut config = load_library_config(&root);
    if !config.categories.iter().any(|item| item == &category) {
        return Err(format!(
            "category `{category}` does not exist. Create it in the app first — the CLI does not \
             invent categories, so a typo cannot silently fork your taxonomy."
        ));
    }

    let stamp = now_iso();
    let mut changed = 0usize;
    let mut missing = 0usize;
    for hash in &hashes {
        match config.images.get_mut(hash) {
            Some(record) => {
                if record.category.as_deref() == Some(category.as_str()) {
                    continue;
                }
                record.category = Some(category.clone());
                record.classified_by = Some("cli".to_string());
                record.classified_at = Some(stamp.clone());
                changed += 1;
            }
            None => missing += 1,
        }
    }
    save_library_config(&root, &config)?;

    if args.json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "category": category,
                "queryMatched": matched,
                "requested": hashes.len(),
                "changed": changed,
                "unchanged": hashes.len() - changed - missing,
                "unknownHashes": missing,
            }))
            .unwrap_or_default()
        );
    } else {
        if matched > 0 {
            println!("`--query` matched {matched} images; all of them are being categorized.");
        }
        println!(
            "{changed} of {} images set to `{category}`{}",
            hashes.len(),
            if missing > 0 {
                format!(" · {missing} hashes are not in this library")
            } else {
                String::new()
            }
        );
        println!("The index now describes the previous categories — rerun `icat index` to refresh it.");
    }
    Ok(())
}

fn cmd_extract(args: &Args) -> Result<(), String> {
    let root = resolve_root(args)?;
    let force = args.is_set("force");
    let limit = args.number("limit", 0)?;

    let view = scan_and_reconcile(&root)?;
    let config = load_library_config(&root);
    let texts = text_dir_path(&root);
    fs::create_dir_all(&texts).map_err(|error| format!("Failed to create text folder: {error}"))?;

    let categories = indexed_categories(&config);
    let mut pending: Vec<(String, String, String)> = pending_text_extraction(&view, &config, force)
        .into_iter()
        // The extraction pass itself is category-blind, but a run fired from here is about the
        // search corpus, so it follows the same scope the index does.
        .filter(|image| {
            args.is_set("all-categories")
                || image
                    .category
                    .as_ref()
                    .map(|category| categories.contains(category))
                    .unwrap_or(false)
        })
        .map(|image| (image.hash.clone(), image.path.clone(), image.name.clone()))
        .collect();
    drop(config);

    if limit > 0 {
        pending.truncate(limit);
    }

    if pending.is_empty() {
        println!("Nothing to extract.");
        return Ok(());
    }

    println!("Extracting text from {} images…", pending.len());
    let total = pending.len();
    let mut results: Vec<(String, u32)> = Vec::new();
    let mut failures = 0usize;

    for (position, (hash, path, name)) in pending.iter().enumerate() {
        match crate::ocr::extract_image_text(Path::new(path)) {
            Ok(text) => {
                let target = text_index::text_file_path(&texts, hash);
                match fs::write(&target, &text) {
                    Ok(()) => results.push((hash.clone(), text.chars().count() as u32)),
                    Err(error) => {
                        failures += 1;
                        eprintln!("  {name}: failed to save text ({error})");
                    }
                }
            }
            Err(error) => {
                failures += 1;
                eprintln!("  {name}: {error}");
            }
        }
        if results.len() >= 250 {
            commit_extraction_results(&root, &mut results)?;
        }
        if (position + 1) % 100 == 0 || position + 1 == total {
            eprintln!("  {}/{}", position + 1, total);
        }
    }
    commit_extraction_results(&root, &mut results)?;

    println!(
        "Extracted {} of {total}{}.",
        total - failures,
        if failures > 0 { format!(", {failures} failed") } else { String::new() }
    );

    // The new text makes the index stale by construction; rebuilding here rather than on the next
    // query keeps the cost inside the run that caused it.
    let config = load_library_config(&root);
    let index = build_text_index_for(&root, &config);
    text_index::save(&root, &index)?;
    println!("Index rebuilt: {} documents.", index.report.docs);
    Ok(())
}

fn cmd_folders(args: &Args) -> Result<(), String> {
    let root = resolve_root(args)?;

    if let Some(path) = args.flag("add") {
        // Adding a source folder changes what the whole app sees, not just this index — so it is
        // announced rather than done quietly, and it goes through the app's own command so the
        // "direct subfolder of the root" rule is enforced in exactly one place.
        let view = crate::add_manual_source_folder(
            root.to_string_lossy().to_string(),
            path.to_string(),
        )?;
        println!("Added source folder `{path}`.");
        println!(
            "The library now has {} source folders and {} images. Run `icat extract` when you want \
             its text, then `icat index`.",
            view.source_folders.len(),
            view.images.len()
        );
        return Ok(());
    }

    if let Some(name) = args.flag("exclude") {
        let _lock = SidecarLock::acquire(&root);
        let mut config = load_library_config(&root);
        if !config.text_index_excluded_folders.iter().any(|item| item == name) {
            config.text_index_excluded_folders.push(name.to_string());
            save_library_config(&root, &config)?;
        }
        println!("`{name}` is now excluded from the text index. Run `icat index` to apply it.");
        return Ok(());
    }

    if let Some(name) = args.flag("include") {
        let _lock = SidecarLock::acquire(&root);
        let mut config = load_library_config(&root);
        config.text_index_excluded_folders.retain(|item| item != name);
        save_library_config(&root, &config)?;
        println!("`{name}` is indexed again. Run `icat index` to apply it.");
        return Ok(());
    }

    let view = scan_and_reconcile(&root)?;
    let config = load_library_config(&root);
    let excluded = &config.text_index_excluded_folders;

    if args.json() {
        let folders: Vec<serde_json::Value> = view
            .source_folders
            .iter()
            .map(|folder| {
                serde_json::json!({
                    "name": folder.name,
                    "images": folder.image_count,
                    "manual": folder.is_manual,
                    "indexed": !excluded.contains(&folder.name),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "folders": folders })).unwrap_or_default()
        );
        return Ok(());
    }

    for folder in &view.source_folders {
        println!(
            "{:<40} {:>7} images  {}{}",
            folder.name,
            folder.image_count,
            if excluded.contains(&folder.name) { "excluded" } else { "indexed " },
            if folder.is_manual { " (manual)" } else { "" }
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------

fn human_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.0}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_take_values_but_switches_stay_switches() {
        let argv: Vec<String> = ["search", "webview2", "--from", "7d", "--json", "--limit", "5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = Args::parse(&argv).unwrap();
        assert_eq!(args.command, "search");
        assert_eq!(args.positional, vec!["webview2".to_string()]);
        assert_eq!(args.flag("from"), Some("7d"));
        assert!(args.json());
        assert_eq!(args.number("limit", 20).unwrap(), 5);
    }

    #[test]
    fn inline_values_and_repeated_flags_both_work() {
        let argv: Vec<String> = ["search", "x", "--folder=A", "--folder", "B", "--budget=0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = Args::parse(&argv).unwrap();
        assert_eq!(args.all("folder"), vec!["A".to_string(), "B".to_string()]);
        assert_eq!(args.number("budget", 6000).unwrap(), 0);
    }

    #[test]
    fn a_switch_before_another_switch_is_not_given_its_name_as_a_value() {
        let argv: Vec<String> = ["search", "x", "--dupes", "--json"].iter().map(|s| s.to_string()).collect();
        let args = Args::parse(&argv).unwrap();
        assert!(args.is_set("dupes"));
        assert!(args.json());
        assert_eq!(args.flag("dupes"), Some("true"));
    }

    // `--full` used to swallow `tauri`, leaving a one-word query and a confidently wrong answer.
    // Every switch must leave the words after it alone.
    #[test]
    fn a_switch_never_eats_a_following_query_word() {
        let argv: Vec<String> = ["search", "--full", "tauri", "webview2"].iter().map(|s| s.to_string()).collect();
        let args = Args::parse(&argv).unwrap();
        assert!(args.is_set("full"));
        assert_eq!(
            args.positional,
            vec!["tauri".to_string(), "webview2".to_string()],
            "both query words must survive the switch"
        );

        for switch in ["dupes", "all", "raw", "json", "group", "force", "hashes", "no-build"] {
            let argv: Vec<String> = ["search", &format!("--{switch}"), "needle"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let args = Args::parse(&argv).unwrap();
            assert_eq!(args.positional, vec!["needle".to_string()], "--{switch} ate the query");
        }
    }

    // A read pages at 20; a write must not. `categorize --query` inheriting the read default meant
    // a 1,471-match query silently edited 20 images and called that the match set.
    #[test]
    fn a_write_takes_every_match_unless_capped_on_purpose() {
        let parse = |tokens: &[&str]| {
            Args::parse(&tokens.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
        };

        let uncapped = parse(&["categorize", "--query", "tauri", "--to", "Archive"]);
        assert_eq!(write_limit(&uncapped).unwrap(), 0, "no cap asked for means all of them");
        assert_eq!(
            query_options_for(&uncapped, "tauri".into(), "until").unwrap().limit,
            20,
            "the shared builder still carries the READ default — which is exactly why the write \
             path has to override it rather than inherit it"
        );

        assert_eq!(write_limit(&parse(&["categorize", "-k", "5"])).unwrap(), 5);
        assert_eq!(write_limit(&parse(&["categorize", "--limit", "5"])).unwrap(), 5);
    }

    // `--to` names a CATEGORY in categorize and a TIME in search. Reading the category through the
    // time parser made `categorize --query … --to <Category>` fail outright, every time.
    #[test]
    fn categorize_reads_its_to_as_a_category_not_as_a_time() {
        let argv: Vec<String> = ["categorize", "--query", "tauri", "--to", "Archive", "--from", "7d"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = Args::parse(&argv).unwrap();

        let options = query_options_for(&args, "tauri".into(), "until")
            .expect("a category name must not be parsed as a time");
        assert_eq!(args.flag("to"), Some("Archive"), "the category survives untouched");
        assert!(options.from.is_some(), "--from still bounds the query");
        assert!(options.to.is_none(), "and no upper bound was asked for");

        // The upper bound is reachable, just under its own name.
        let argv: Vec<String> = ["categorize", "--query", "x", "--to", "Archive", "--until", "2026-08-03"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = Args::parse(&argv).unwrap();
        let options = query_options_for(&args, "x".into(), "until").unwrap();
        assert_eq!(
            text_index::format_datetime(options.to.unwrap()),
            "2026-08-03 23:59",
            "--until is inclusive of its own day, like --to is elsewhere"
        );

        // And the reading commands are unchanged.
        let argv: Vec<String> = ["search", "x", "--to", "2026-08-03"].iter().map(|s| s.to_string()).collect();
        let args = Args::parse(&argv).unwrap();
        assert!(query_options(&args, "x".into()).unwrap().to.is_some());
    }

    #[test]
    fn a_value_flag_still_takes_the_token_after_it() {
        let argv: Vec<String> = ["search", "needle", "--from", "7d", "--budget", "0", "--folder", "Root"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = Args::parse(&argv).unwrap();
        assert_eq!(args.positional, vec!["needle".to_string()]);
        assert_eq!(args.flag("from"), Some("7d"));
        assert_eq!(args.number("budget", 6000).unwrap(), 0);
        assert_eq!(args.all("folder"), vec!["Root".to_string()]);
    }

    #[test]
    fn a_bare_to_date_covers_the_whole_day() {
        let start = parse_when("2026-08-03", false).unwrap();
        let end = parse_when("2026-08-03", true).unwrap();
        assert_eq!(end - start, 86_399, "--to must be inclusive of its own day");
    }

    #[test]
    fn explicit_times_are_read_in_both_separators() {
        let with_t = parse_when("2026-08-03T14:30", false).unwrap();
        let with_space = parse_when("2026-08-03 14:30", false).unwrap();
        assert_eq!(with_t, with_space);
        assert_eq!(text_index::format_datetime(with_t), "2026-08-03 14:30");
    }

    #[test]
    fn relative_windows_are_read_as_offsets_from_now() {
        let now = local_now();
        let day_ago = parse_when("1d", false).unwrap();
        assert!((now - day_ago - 86_400).abs() <= 2, "1d should be a day back");
    }

    #[test]
    fn an_unreadable_time_is_an_error_not_a_silent_zero() {
        assert!(parse_when("last tuesday", false).is_err());
    }
}
