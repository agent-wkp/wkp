//! `wkp hooks`: prints the exact hook text for a harness to apply
//! (design 3.3).

/// Text shared between the bare-invocation and unknown-`--framework`
/// error messages: what's actually supported today, and where to look
/// for everything else. Kept as one constant so the two error sites
/// below can't drift apart on which frameworks are real.
const SUPPORTED_FRAMEWORKS_HELP: &str = "Currently supported: claude_code. Codex, OpenCode, \
     and other AGENTS.md-reading harnesses don't need this command at all -- see \
     README.md's \"Configure your coding agent\" section for the (different, but \
     equally simple) way to wire those up.";

/// Parses `wkp hooks --framework <name>`.
pub(crate) fn parse_hooks_args(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut framework: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--framework" => {
                framework = Some(args.next().ok_or("--framework requires a value")?);
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    framework.ok_or_else(|| {
        format!(
            "hooks requires --framework <name>, e.g. `wkp hooks --framework claude_code`. \
             {SUPPORTED_FRAMEWORKS_HELP}"
        )
    })
}

/// `wkp hooks --framework claude_code`: prints the exact Claude Code
/// `SessionStart` hook text (design 3.3: "the only 'installer'... prints
/// the exact hook text for an agent or a human to apply") -- applying it
/// to `.claude/settings.local.json` is left to the caller. Re-indexes
/// quietly and best-effort (`|| true`: a broken index must never block
/// the session).
pub(crate) fn render_hooks(framework: &str) -> Result<String, String> {
    match framework {
        "claude_code" => Ok(CLAUDE_CODE_HOOK.to_string()),
        other => Err(format!(
            "unknown --framework value '{other}'. {SUPPORTED_FRAMEWORKS_HELP}"
        )),
    }
}

const CLAUDE_CODE_HOOK: &str = r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "wkp index >/dev/null 2>&1 || true; cat .wkp/tier0.md 2>/dev/null || true"
          }
        ]
      }
    ]
  }
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_dir;

    #[test]
    fn render_hooks_claude_code_matches_golden_output() {
        let output = render_hooks("claude_code").expect("render_hooks");
        assert_eq!(output, CLAUDE_CODE_HOOK);
        assert!(output.contains("SessionStart"));
        assert!(output.contains("wkp index"));
        assert!(output.contains("tier0.md"));
    }

    /// Regression test for a real usability gap (William, 2026-09-17):
    /// `wkp hooks --framework codex` used to say only "unknown
    /// --framework value: codex (expected: claude_code)" -- true, but it
    /// didn't tell a Codex/OpenCode user there's a real, documented
    /// workaround for them, just that this specific command doesn't
    /// support their harness. The message now names what *is* supported
    /// and points at the actual alternative rather than leaving the
    /// reader to go searching.
    #[test]
    fn render_hooks_rejects_unknown_framework_with_a_helpful_message() {
        let err = render_hooks("codex").expect_err("codex is not a supported framework");
        assert!(
            err.contains("claude_code"),
            "should name what is supported: {err}"
        );
        assert!(
            err.contains("AGENTS.md"),
            "should point at the real workaround for non-claude_code harnesses: {err}"
        );
    }

    #[test]
    fn parse_hooks_args_bare_invocation_names_the_supported_framework() {
        let err =
            parse_hooks_args(std::iter::empty()).expect_err("bare `wkp hooks` has no framework");
        assert!(err.contains("claude_code"), "got: {err}");
        assert!(err.contains("AGENTS.md"), "got: {err}");
    }

    #[test]
    fn hooks_never_writes_any_file() {
        let temp = temp_dir("hooks-no-write");
        let dir = temp.path();
        let before = std::fs::read_dir(dir).unwrap().count();

        // render_hooks takes no path at all, so there is structurally
        // nothing for it to write to; this asserts the observable
        // consequence (no new file appears anywhere near it) rather than
        // relying solely on the type signature.
        let _ = render_hooks("claude_code");

        let after = std::fs::read_dir(dir).unwrap().count();
        assert_eq!(before, after, "wkp hooks must never write a file");
    }
}
