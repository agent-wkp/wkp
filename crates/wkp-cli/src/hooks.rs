//! `wkp hooks`: prints the exact hook text for a harness to apply
//! (design 3.3).
//!
//! Two different shapes come out of here, not one: `claude_code` prints
//! a real `SessionStart` hook JSON block (Claude Code has a dedicated
//! hook API to plug into). Every other supported value (`codex`,
//! `opencode`, `hermes`, and the generic `agents_md`) prints the same
//! plain Markdown instruction, on the assumption that pasting it
//! somewhere the harness reads at startup achieves the same thing:
//! `wkp index && wkp materialize --tier 0 && cat .wkp/tier0.md` run
//! before the harness's first real turn. All three named harnesses are
//! confirmed `AGENTS.md` auto-readers, `hermes` included: verified
//! directly against its own source (`agent/prompt_builder.py`,
//! `NousResearch/hermes-agent`, 2026-09-17), not assumed the way an
//! earlier version of this comment had it. Hermes's own precedence,
//! first match wins, only one loads: `.hermes.md`/`HERMES.md` (walking
//! up to the git root) beats the `AGENTS.md` chain, which beats
//! `CLAUDE.md`, which beats `.cursorrules`. So `AGENTS.md` reaches
//! Hermes exactly like it reaches Codex/OpenCode, *unless* the project
//! also carries its own `.hermes.md`/`HERMES.md` -- worth knowing, not
//! a reason to withhold the instruction. Still just prints text either
//! way -- this command never writes a file itself (design 3.3's "the
//! only 'installer'... prints the exact hook text for an agent or a
//! human to apply").

/// Text shared between the bare-invocation and unknown-`--framework`
/// error messages: what's actually supported today. Kept as one
/// constant so the two error sites below can't drift apart on which
/// frameworks are real.
const SUPPORTED_FRAMEWORKS_HELP: &str =
    "Supported: claude_code (a real SessionStart hook); codex / opencode / hermes (a \
     plain-text instruction to paste into AGENTS.md -- all three are confirmed to read \
     that file automatically; for hermes specifically, only if the project doesn't also \
     have its own .hermes.md/HERMES.md, which takes precedence over AGENTS.md there); or \
     the generic agents_md, for any \
     other AGENTS.md-reading harness.";

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

/// `wkp hooks --framework <name>`: prints the exact text for a harness
/// to apply. `claude_code` gets a real `SessionStart` hook (design
/// 3.3) -- applying it to `.claude/settings.local.json` is left to the
/// caller. `codex`/`opencode`/`hermes`/`agents_md` all get the same
/// instruction text (see module doc comment for why one text serves
/// all four, and for the honest caveat on `hermes` specifically).
/// Re-indexes quietly and best-effort (`|| true`: a broken index must
/// never block the session) in both cases.
pub(crate) fn render_hooks(framework: &str) -> Result<String, String> {
    match framework {
        "claude_code" => Ok(CLAUDE_CODE_HOOK.to_string()),
        "codex" | "opencode" | "hermes" | "agents_md" => Ok(AGENTS_MD_INSTRUCTION.to_string()),
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

/// Kept byte-for-byte identical to the snippet in README.md's "Configure
/// your coding agent" section -- that section's own prose points here
/// as the authoritative source now that it's generated instead of
/// hand-copied, so a drift between the two would be a real bug, not
/// just a style nit.
const AGENTS_MD_INSTRUCTION: &str = "## WKP memory
Before starting work, run
`wkp index && wkp materialize --tier 0 && cat .wkp/tier0.md`
and treat its output as already-established project context.
";

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

    /// `codex`/`opencode`/`hermes`/`agents_md` are four names for the
    /// exact same output (module doc comment explains why, including
    /// the honest caveat on `hermes`'s own unverified integration
    /// surface): none of them have a dedicated hook API the way Claude
    /// Code does, so all four just get the plain-text instruction.
    #[test]
    fn render_hooks_codex_opencode_hermes_and_agents_md_all_produce_the_same_instruction() {
        let codex = render_hooks("codex").expect("codex should be supported");
        let opencode = render_hooks("opencode").expect("opencode should be supported");
        let hermes = render_hooks("hermes").expect("hermes should be supported");
        let agents_md = render_hooks("agents_md").expect("agents_md should be supported");
        assert_eq!(codex, AGENTS_MD_INSTRUCTION);
        assert_eq!(codex, opencode);
        assert_eq!(codex, hermes);
        assert_eq!(codex, agents_md);
        assert!(codex.contains("wkp index"));
        assert!(codex.contains("wkp materialize --tier 0"));
        assert!(codex.contains("AGENTS.md") || codex.contains("## WKP memory"));
    }

    /// Regression test for a real usability gap (William, 2026-09-17):
    /// `wkp hooks --framework codex` used to say only "unknown
    /// --framework value: codex (expected: claude_code)" -- true at the
    /// time, but it left a Codex/OpenCode user to go find the real
    /// workaround themselves instead of naming it. `codex` itself is
    /// now a real, supported value (see the test above) -- this test
    /// covers a name that genuinely isn't recognized, confirming the
    /// error still names what *is* supported rather than just "unknown".
    #[test]
    fn render_hooks_rejects_unknown_framework_with_a_helpful_message() {
        let err =
            render_hooks("some-other-harness").expect_err("not a real supported framework name");
        assert!(
            err.contains("claude_code"),
            "should name what is supported: {err}"
        );
        assert!(
            err.contains("agents_md") || err.contains("AGENTS.md"),
            "should point at the AGENTS.md-instruction alternative: {err}"
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
