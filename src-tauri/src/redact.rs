//! Credential scrubbing for text on its way *out* of this machine's screenshot history.
//!
//! The OCR corpus is a transcript of everything that has been on screen, so it contains whatever
//! was on screen — API keys in a settings file, a bearer token in a devtools pane, an address in a
//! mail client. The rule settled with the user: **index raw, redact at read.**
//!
//! - The index keeps real terms, so a key stays *findable* ("which screenshot leaked this?").
//! - Every consumer-facing read — the CLI, the Tauri search commands, anything Hermes or an agent
//!   touches — goes through [`redact`] first.
//! - The app's own detail pane and an explicit `--raw` are the only paths that see the original,
//!   because a human looking at their own screenshot is not an egress event.
//!
//! Redacting at read rather than at index time is what lets this pattern list grow without a
//! rebuild: a pattern added today covers text extracted last month on the very next query.
//!
//! **Every credential in this module's tests is synthetic, and must stay that way.** The tests read
//! naturally as a place for a "real" example — the pattern list is only convincing against input
//! that looks genuine — and that is exactly how the live `visionApiKey` came to sit in
//! `removes_the_apps_own_key_shape` and be pushed to a public repo. Match the *shape* of a
//! credential; never its value. A fixture is source, and source here is published.

use regex::Regex;
use std::sync::OnceLock;

/// A named pattern and the label that replaces what it matches. The label is deliberately visible
/// (`[redacted:token]`) rather than a blank or a row of asterisks — a reader, human or model, should
/// be able to tell the difference between "nothing was there" and "something was removed here".
struct Rule {
    kind: &'static str,
    pattern: Regex,
    /// Capture group holding the part to blank. `0` means the whole match; a higher number keeps the
    /// label in place (`api_key = [redacted:assignment]`) so the surrounding line still reads.
    group: usize,
}

fn rules() -> &'static Vec<Rule> {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut rules = Vec::new();
        let mut push = |kind: &'static str, pattern: &str, group: usize| {
            if let Ok(pattern) = Regex::new(pattern) {
                rules.push(Rule { kind, pattern, group });
            }
        };

        // Provider-shaped keys. `sk-` covers the LM Studio and OpenAI-style tokens this machine
        // actually holds; the app's own `visionApiKey` is exactly this shape.
        push("key", r"\bsk-[A-Za-z0-9_\-]{6,}(?::[A-Za-z0-9_\-]{6,})?", 0);
        push("key", r"\bAKIA[0-9A-Z]{16}\b", 0);
        push("key", r"\bghp_[A-Za-z0-9]{20,}\b", 0);
        push("key", r"\bxox[baprs]-[A-Za-z0-9\-]{10,}", 0);

        // `secret = value`, in whatever punctuation the surrounding UI used. Only the value goes.
        push(
            "assignment",
            r"(?i)\b(api[_\- ]?key|apikey|access[_\- ]?token|auth[_\- ]?token|client[_\- ]?secret|secret|password|passwd|pwd|token)\b\s*[:=]\s*([^\s,;{}\[\]\x22']{4,})",
            2,
        );

        push("token", r"(?i)\bbearer\s+([A-Za-z0-9._\-]{16,})", 1);

        // A JWT is three base64url segments; matched on its own because the generic long-run rule
        // below would only catch part of it.
        push("token", r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\b", 0);

        push("private-key", r"-----BEGIN[A-Z ]*PRIVATE KEY-----", 0);

        // Long undifferentiated runs: hashes, session ids, base64 blobs. The floors are high enough
        // that ordinary prose, file names and code identifiers never reach them.
        push("hex", r"\b[A-Fa-f0-9]{40,}\b", 0);
        push("blob", r"\b[A-Za-z0-9+/]{56,}={0,2}\b", 0);

        push("email", r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,24}\b", 0);

        rules
    })
}

/// What a redaction pass removed, so a caller can say so instead of silently handing back
/// something shorter than what is on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionReport {
    pub replacements: usize,
}

impl RedactionReport {
    pub fn is_empty(&self) -> bool {
        self.replacements == 0
    }
}

/// Scrubs credential-shaped runs out of `text`. Returns the cleaned text and a count.
pub fn redact(text: &str) -> (String, RedactionReport) {
    let mut out = text.to_string();
    let mut report = RedactionReport::default();

    for rule in rules() {
        // Collected first, then applied right-to-left, so replacing one match cannot shift the byte
        // offsets of the ones still to come.
        let spans: Vec<(usize, usize)> = rule
            .pattern
            .captures_iter(&out)
            .filter_map(|captures| captures.get(rule.group).map(|m| (m.start(), m.end())))
            .collect();
        if spans.is_empty() {
            continue;
        }
        let label = format!("[redacted:{}]", rule.kind);
        for (start, end) in spans.into_iter().rev() {
            out.replace_range(start..end, &label);
            report.replacements += 1;
        }
    }

    (out, report)
}

/// Convenience for the common case where only the text is wanted.
pub fn redact_text(text: &str) -> String {
    redact(text).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_the_apps_own_key_shape() {
        // A synthetic stand-in built to the shape LM Studio issues (`sk-lm-<id>:<secret>`), which
        // is the shape this app's own `visionApiKey` has. It must stay synthetic: a real key used
        // as a fixture here is a real key published to this repo, and one was — see the note at the
        // top of the module. Never paste a live value in to make the assertion more convincing.
        let (out, report) = redact("visionApiKey: sk-lm-EXAMPLE1:NOTAREALKEY000000 done");
        assert!(!out.contains("NOTAREALKEY000000"), "{out}");
        assert!(out.contains("done"), "surrounding text must survive: {out}");
        assert!(!report.is_empty());
    }

    #[test]
    fn keeps_the_label_and_drops_only_the_value() {
        let out = redact_text("password = hunter2hunter2");
        assert!(out.starts_with("password ="), "the line must stay readable: {out}");
        assert!(!out.contains("hunter2hunter2"));
    }

    #[test]
    fn scrubs_bearer_tokens_and_jwts() {
        let out = redact_text("Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345");
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz012345"), "{out}");
        let jwt = redact_text("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N");
        assert!(jwt.contains("[redacted:"), "{jwt}");
    }

    #[test]
    fn leaves_ordinary_screenshot_prose_alone() {
        // This is the failure mode that matters: over-redaction makes the whole corpus useless.
        let text = "vault-search serves 127.0.0.1:5278 with /api/search?q=&k=&format=text · warm start 148 ms";
        assert_eq!(redact_text(text), text);
    }

    #[test]
    fn leaves_identifiers_and_hashes_shorter_than_the_floor() {
        // Content hashes in this app are 16 hex chars and are the primary key everywhere — they
        // must never be scrubbed, or every result would come back unusable.
        let text = "hash 305549d0aaceb3ad group 369441f8e7f00db4";
        assert_eq!(redact_text(text), text);
    }

    #[test]
    fn redacts_email_addresses() {
        let out = redact_text("mail from someone@example.com about the build");
        assert!(!out.contains("someone@example.com"), "{out}");
        assert!(out.contains("about the build"));
    }

    #[test]
    fn is_idempotent() {
        let once = redact_text("token: abcd1234efgh5678ijkl");
        assert_eq!(redact_text(&once), once, "a second pass must not chew its own labels");
    }
}
