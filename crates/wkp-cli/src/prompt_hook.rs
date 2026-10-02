//! `wkp prompt-hook`: Claude Code `UserPromptSubmit` hook plumbing
//! (registered by `wkp hooks --framework claude_code`; see AGENTS.md's
//! "Automatic prompt-time discovery"). Reads the hook's own JSON payload
//! from stdin, pulls out the `prompt` field, and -- if a search against
//! it clears `min_score` -- prints a short block of candidate paths as
//! additional context, the same way `SessionStart`'s own embedded
//! command already surfaces tier 0.
//!
//! Best-effort throughout, by design, not by omission: a missing index,
//! a malformed payload, an unparseable field, or an empty result set all
//! mean "print nothing" here, and the embedded hook command this feeds
//! (`hooks.rs`'s `CLAUDE_CODE_HOOK`) wraps the whole call in `|| true` on
//! top of that -- a user's prompt must never be blocked or delayed by
//! this command misbehaving.

use std::path::PathBuf;

pub(crate) struct PromptHookOptions {
    pub(crate) path: PathBuf,
    pub(crate) limit: usize,
    pub(crate) min_score: f64,
}

const DEFAULT_LIMIT: usize = 5;

/// BM25 scores (`-bm25(...)`, see `wkp-core`'s `search`) are unbounded
/// and corpus-dependent, not a normalized 0..1 range -- this default is
/// a starting heuristic picked without a real corpus to calibrate
/// against, not a measured value. `WKP_PROMPT_HOOK_MIN_SCORE` overrides
/// it; the right way to pick a better number is the golden-query/
/// recall@5 harness flagged as follow-up work in the same
/// discovery-improvements pass this command landed in (see
/// AGENTS.md).
const DEFAULT_MIN_SCORE: f64 = 0.01;

pub(crate) fn prompt_hook_options_from_env(path: PathBuf) -> PromptHookOptions {
    let min_score = std::env::var("WKP_PROMPT_HOOK_MIN_SCORE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_MIN_SCORE);
    PromptHookOptions {
        path,
        limit: DEFAULT_LIMIT,
        min_score,
    }
}

/// Runs the hook end to end. `stdin_payload` is the hook event's raw
/// JSON (Claude Code's own `UserPromptSubmit` contract: the hook
/// receives the whole event object on stdin; this only ever reads its
/// `prompt` field). Always returns a `String`, never an error -- an
/// empty string means "nothing to inject," which covers every failure
/// mode (bad JSON, no store, no index, no hits, nothing above
/// `min_score`) identically, since none of them should ever surface as
/// a visible failure from a hook that must never block the prompt it's
/// attached to.
pub(crate) fn run_prompt_hook(stdin_payload: &str, opts: &PromptHookOptions) -> String {
    let Some(prompt) = extract_json_string_field(stdin_payload, "prompt") else {
        return String::new();
    };
    if prompt.trim().is_empty() {
        return String::new();
    }
    let Ok(conn) = wkp_core::index::open_index(&opts.path.join(".wkp/index.db")) else {
        return String::new();
    };
    let filter = wkp_core::index::SearchFilter {
        limit: Some(opts.limit),
        ..Default::default()
    };
    let Ok(hits) = wkp_core::index::search_any_term(&conn, &prompt, &filter) else {
        return String::new();
    };
    let relevant: Vec<_> = hits
        .into_iter()
        .filter(|h| h.score >= opts.min_score)
        .collect();
    if relevant.is_empty() {
        return String::new();
    }
    format!(
        "## wkp: possibly relevant context\n\n{}\n\nRead one of these, or run `wkp context \"<topic>\"` to go deeper.",
        crate::search::format_text(&relevant)
    )
}

/// Finds the string value of a top-level `"field":"..."` entry in
/// `input`, decoding JSON string escapes. Hand-rolled rather than
/// pulling in `serde_json` for one field -- same slim-core reasoning as
/// `search::parse_search_args`'s own doc comment on not adding `clap`
/// yet. Correctly skips over *every* string literal it scans past, not
/// just the one it is looking for, via [`read_json_string`] -- so a
/// value elsewhere in the payload that happens to contain the literal
/// text `"field"` (plausible here: `prompt` is arbitrary user text, and
/// could itself contain the substring `"prompt"`) can never be mistaken
/// for the real key. A match requires the string to actually be
/// positioned as a JSON string *token*, immediately followed by `:`.
pub(crate) fn extract_json_string_field(input: &str, field: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let (value, end) = read_json_string(input, i)?;
        let mut j = end;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if value == field && bytes.get(j) == Some(&b':') {
            j += 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            return if bytes.get(j) == Some(&b'"') {
                read_json_string(input, j).map(|(v, _)| v)
            } else {
                None
            };
        }
        i = end;
    }
    None
}

/// Decodes one JSON string literal in `s` starting at byte offset
/// `start` (`s.as_bytes()[start]` must be `"`), returning the decoded
/// content and the byte offset just past the closing quote. `None` on
/// malformed input (unterminated string, a lone trailing backslash, a
/// truncated `\u` escape, or an unknown escape character) -- every
/// caller here treats that as "field not found," never a panic.
///
/// Operates on byte offsets but only ever slices `s` at the boundaries
/// of single-byte ASCII characters (`"`, `\`, and the fixed-width
/// escape sequences) -- those can never fall inside a multi-byte UTF-8
/// sequence's continuation bytes (always `>= 0x80`), so every slice
/// taken is a valid `&str` boundary; a run of ordinary bytes between
/// two such markers is pushed as one slice rather than decoded
/// byte-by-byte, which is what keeps this correct for non-ASCII content
/// (a naive `byte as char` cast per byte would mangle it).
fn read_json_string(s: &str, start: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = start + 1;
    let mut run_start = i;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                out.push_str(&s[run_start..i]);
                return Some((out, i + 1));
            }
            b'\\' => {
                out.push_str(&s[run_start..i]);
                let esc = *bytes.get(i + 1)?;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'u' => {
                        let hex = s.get(i + 2..i + 6)?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        i += 6;
                        run_start = i;
                        continue;
                    }
                    _ => return None,
                }
                i += 2;
                run_start = i;
            }
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    #[test]
    fn extract_json_string_field_reads_a_flat_field() {
        let payload = r#"{"session_id":"abc","hook_event_name":"UserPromptSubmit","prompt":"guardrails nemo config"}"#;
        assert_eq!(
            extract_json_string_field(payload, "prompt").as_deref(),
            Some("guardrails nemo config")
        );
    }

    #[test]
    fn extract_json_string_field_decodes_escapes() {
        let payload = r#"{"prompt":"line one\nline two \"quoted\" and \\backslash"}"#;
        assert_eq!(
            extract_json_string_field(payload, "prompt").as_deref(),
            Some("line one\nline two \"quoted\" and \\backslash")
        );
    }

    /// The realistic adversarial case: the prompt text itself contains
    /// the literal substring `"prompt"` before the real key is reached.
    /// A naive substring search for `"prompt"` would stop there and
    /// misparse; this must keep scanning to the real key.
    #[test]
    fn extract_json_string_field_is_not_fooled_by_the_field_name_appearing_in_an_earlier_value() {
        let payload =
            r#"{"cwd":"/home/x/\"prompt\" is a confusing dir name","prompt":"real value"}"#;
        assert_eq!(
            extract_json_string_field(payload, "prompt").as_deref(),
            Some("real value")
        );
    }

    #[test]
    fn extract_json_string_field_returns_none_when_absent() {
        assert_eq!(
            extract_json_string_field(r#"{"other":"x"}"#, "prompt"),
            None
        );
    }

    #[test]
    fn extract_json_string_field_returns_none_on_malformed_json() {
        assert_eq!(
            extract_json_string_field(r#"{"prompt":"unterminated"#, "x"),
            None
        );
    }

    #[test]
    fn run_prompt_hook_returns_empty_when_prompt_field_missing() {
        let temp = temp_dir("prompt-hook-missing-field");
        let opts = PromptHookOptions {
            path: temp.path().to_path_buf(),
            limit: 5,
            min_score: 0.0,
        };
        assert_eq!(run_prompt_hook(r#"{"other":"x"}"#, &opts), "");
    }

    #[test]
    fn run_prompt_hook_returns_empty_when_no_store_exists_at_path() {
        let temp = temp_dir("prompt-hook-no-store");
        let opts = PromptHookOptions {
            path: temp.path().to_path_buf(),
            limit: 5,
            min_score: 0.0,
        };
        assert_eq!(run_prompt_hook(r#"{"prompt":"anything"}"#, &opts), "");
    }

    #[test]
    fn run_prompt_hook_finds_and_formats_a_relevant_hit() {
        let temp = temp_dir("prompt-hook-hit");
        let dir = temp.path();
        crate::test_support::test_init(dir).expect("run_init");
        std::fs::write(
            dir.join("a.md"),
            "---\ntitle: Guardrails config\ntype: knowledge\n---\n\nNeMo guardrails setup notes\n",
        )
        .expect("write a.md");
        wkp_git::commit_all(dir, "seed").expect("commit_all");
        crate::index_cmd::run_index(dir).expect("run_index");

        let opts = PromptHookOptions {
            path: dir.to_path_buf(),
            limit: 5,
            min_score: 0.0,
        };
        let payload = r#"{"hook_event_name":"UserPromptSubmit","prompt":"how do I configure guardrails for this project"}"#;
        let output = run_prompt_hook(payload, &opts);
        assert!(output.contains("Guardrails config"), "got: {output}");
        assert!(output.contains("a.md"), "got: {output}");
        assert!(output.contains("wkp context"), "got: {output}");
    }

    #[test]
    fn run_prompt_hook_suppresses_output_below_min_score() {
        let temp = temp_dir("prompt-hook-below-threshold");
        let dir = temp.path();
        crate::test_support::test_init(dir).expect("run_init");
        std::fs::write(
            dir.join("a.md"),
            "---\ntitle: Guardrails config\ntype: knowledge\n---\n\nNeMo guardrails setup notes\n",
        )
        .expect("write a.md");
        wkp_git::commit_all(dir, "seed").expect("commit_all");
        crate::index_cmd::run_index(dir).expect("run_index");

        let opts = PromptHookOptions {
            path: dir.to_path_buf(),
            limit: 5,
            min_score: 1_000_000.0,
        };
        let payload = r#"{"prompt":"how do I configure guardrails for this project"}"#;
        assert_eq!(run_prompt_hook(payload, &opts), "");
    }
}
