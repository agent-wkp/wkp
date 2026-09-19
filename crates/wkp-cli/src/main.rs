#![forbid(unsafe_code)]

//! The `wkp` binary. Subcommands (`init`, `index`, `search`, `context`,
//! `materialize`, `hooks`, `remember`, `wkpd`, ...) land starting in M1;
//! see `docs/plan/milestones.md`.

use std::path::{Path, PathBuf};

mod bundle;
mod context;
mod filter;
mod forget;
mod hooks;
mod hub_register;
mod import;
mod index_cmd;
mod init;
mod materialize;
mod merge_driver;
mod podman_sandbox;
mod promote;
mod purge;
mod remember;
mod resolve_conflicts;
mod sandbox;
mod search;
mod sync_cmd;
mod usage_cmd;
mod wkpd;

#[cfg(test)]
mod test_support;

/// M6-1 (issue #170, design 7.5): restricts write-class filesystem
/// access to `store_path` (plus `/dev/null`, see below) before a
/// write-heavy subcommand does any real I/O. Non-fatal on error -- an
/// actual `Err` here (not the already-handled "kernel doesn't support
/// Landlock" case, which `sandbox::restrict_writes_to` itself logs and
/// treats as success) means the sandbox setup itself hit something
/// unexpected; failing the whole command over a brand-new hardening
/// layer would trade a real functional regression for a marginal,
/// unproven security gain, backwards from design 7.5's own "hardening,
/// not a hard requirement" framing.
///
/// `/dev/null` has to be in the allowed list, not just `store_path`:
/// found by hand (this restriction had never actually been exercised
/// against a real `git` subprocess before -- `sandbox_integration.rs`'s
/// own tests only ever drove the two hidden self-test subcommands,
/// neither of which spawns `git`). Every `git` invocation, regardless
/// of subcommand -- even a pure read like `git ls-files` -- opens
/// `/dev/null` with read+write access during its own startup
/// (`sanitize_stdfds()`, filling any of its own fds 0/1/2 that aren't
/// already open valid descriptors), and dies with "could not open
/// '/dev/null' for reading and writing" if that's denied. Without this,
/// the write-sandbox didn't harden `wkp index`/`remember`/`promote`/
/// `forget` -- it broke all of them outright on any kernel actually
/// enforcing it, since `wkp-git`'s every real subprocess call runs
/// after this restriction is applied. `/dev/null` carries no
/// interesting write-access security property of its own (writes to it
/// are already discarded), so allowing it doesn't meaningfully weaken
/// the restriction's real intent -- confining writes to files that
/// matter.
fn apply_write_sandbox(store_path: &Path) {
    if let Err(e) = sandbox::restrict_writes_to(&[store_path, Path::new("/dev/null")]) {
        eprintln!("wkp: write-sandbox setup failed, continuing without it: {e}");
    }
}

/// The real, current subcommand list -- printed for bare `wkp`, `wkp
/// --help`/`-h`/`help`, and (prefixed with an "unknown subcommand"
/// line) an unrecognized subcommand. Kept as one literal string next to
/// `main`'s own dispatch `match` rather than generated from it, so
/// adding a subcommand there is a visible two-line diff here too --
/// this text drifting from the real dispatch list silently is exactly
/// the bug class that left the old "no subcommands implemented yet"
/// placeholder (an M0-era stub, predating every subcommand below)
/// shipped all the way through v1.0.0/v1.0.1's real public releases.
/// `__sandbox-self-test-*` are deliberately excluded (see their own
/// doc comment below): internal test-only hooks, not user subcommands.
const USAGE: &str = "\
Usage: wkp <COMMAND> [ARGS]

Commands:
  init                   Initialize a store in the current directory
  index                  Build or refresh the SQLite FTS5 index
  search <query>         BM25 search over the index
  context <query>        Search plus graph-traversal context
  traverse <path>        Walk refs:/wikilink graph edges from one item
  materialize --tier N   Write tier0.md/tier1.md to .wkp/
  hooks --framework F    Print SessionStart hook text for a harness
  remember               Write an agent-authored item to inbox/
  promote <path>         Move an inbox/ item into the durable tree (human-signed)
  forget                 Remove an item / rotate encryption recipients
  purge <path>           Erase a path from git history (wraps git-filter-repo)
  sync [status]          Sync with configured remotes / report conflicts
  bundle export|import   Air-gapped sync via git bundle
  import                 Import ~/.claude/projects, CLAUDE.md, AGENTS.md
  merge-driver           Git merge-driver plumbing (registered by init)
  filter clean|smudge    Git clean/smudge filter plumbing (registered by init)
  resolve-conflicts      Resolve a modify/delete conflict pair
  hub register           Register this device with a wkp-hub
  wkpd                   Watch-triggered sync daemon
  usage                  Show local usage metrics (calls, latency, errors)

  --version, -V          Print the version and exit
  --help, -h, help       Print this message and exit

  --sandbox <backend> <COMMAND> [ARGS]
                         Run COMMAND inside a container instead of
                         natively (ADR-0015). Backends: podman.

See AGENTS.md for how an agent should use these, or docs/plan/milestones.md
for what each one's own acceptance criteria are.\
";

/// True when a subcommand's own remaining arguments (not including the
/// subcommand name itself) ask for help -- checked in `dispatch` before
/// that subcommand's own hand-rolled parser ever sees the args, so every
/// subcommand supports `--help`/`-h` without each of the ~17 independent
/// parsers (CLAUDE.md's slim-core: still no `clap`) needing to
/// special-case it themselves. Found by hand: `wkp init --help` used to
/// be silently interpreted as `wkp init` with path `./--help`, since
/// `init`/`import` (unlike every other subcommand) had no argument
/// parser at all -- see their own dispatch arms below for the fix.
fn wants_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

const HELP_INIT: &str = "\
Usage: wkp init [PATH]

Makes PATH (default: current directory) a git repository with a
.wkp/ directory, wires SSH commit signing, registers the merge driver
and clean/smudge filter, and runs `wkp import` once. Idempotent --
safe to re-run.";

const HELP_IMPORT: &str = "\
Usage: wkp import [PATH]

One-shot, idempotent migration of pre-existing memory into
inbox/import/: CLAUDE.md/AGENTS.md at the store root, and every
~/.claude/projects/*/memory/*.md file. Runs automatically as part of
`wkp init`; call it again by hand if new source files show up later.
An existing destination file is never overwritten.";

const HELP_SEARCH: &str = "\
Usage: wkp search <query> [--tier N] [--budget N] [-k/--limit N]
                   [--format text|paths|json] [--path DIR]
                   [--embed-url URL [--embed-model NAME] [--embed-key-file PATH]]

BM25 full-text search over index.db. --budget stops once estimated
token cost exceeds N. --format paths is the form to feed into a
Read-tool call. --embed-url opts into hybrid BM25+embedding search
(requires a binary built with the `embed` feature); never called by
default.";

const HELP_CONTEXT: &str = "\
Usage: wkp context <query> [--tier N] [--budget N] [-k/--limit N]
                    [--format text|paths|json] [--path DIR]

Same flags as `search` (embedding flags are search-only -- context
rejects them explicitly). Adds a graph traversal of the refs:/wikilink
edges from each hit, so you also get directly-linked neighbors.";

const HELP_TRAVERSE: &str = "\
Usage: wkp traverse <path> [--depth N] [--format text|paths|json] [--path DIR]

Walks refs:/[[wikilink]] edges outward from one specific file, no
search query involved. --depth defaults to 2.";

const HELP_MATERIALIZE: &str = "\
Usage: wkp materialize --tier N [--path DIR]

Writes .wkp/tierN.md atomically (temp file + rename). --tier is
required -- no default, so a session-start hook never materializes
\"whatever tier\" by accident.";

const HELP_INDEX: &str = "\
Usage: wkp index [PATH] [--embed-url URL] [--embed-model NAME] [--embed-key-file PATH]

Rescans the store and updates index.db incrementally, using git's own
change detection to hash only what changed. Only .md files are
indexed. Safe to run any time; cheap after a single-file edit.";

const HELP_USAGE: &str = "\
Usage: wkp usage [--window 1h|1d|7d|30d] [--tool NAME] [--path DIR] [--json]

Reads .wkp/metrics.db back: per-tool invocation count, error count,
and avg/min/max latency for every subcommand run against this store.
--window defaults to 1d; --tool filters to one metric by name.";

const HELP_REMEMBER: &str = "\
Usage: wkp remember --type <type> --principal <principal>
                     --signing-key-file <path> [--scope <scope>]
                     [--title <title>] [--session <session>] [--path <dir>]

Writes one SSH-signed item to inbox/, body read from stdin (never
argv). --type, --principal, and --signing-key-file are required.
Always lands at confidence: proposed, tier 2 -- nothing self-promotes
to tier 0/1.";

const HELP_PROMOTE: &str = "\
Usage: wkp promote <inbox-path> [--to <dest-path>] --principal <principal>
                    --signing-key-file <path> [--path <dir>]

Moves one inbox/ item into the durable tree with a human-signed
commit. The one opt-in exception: a principal explicitly listed under
[promote] auto = [...] in .wkp/config.toml.";

const HELP_FORGET: &str = "\
Usage: wkp forget <path> --principal <p> --signing-key-file <f> [--path <dir>]
   or: wkp forget --device <id> --principal <p> --signing-key-file <f> [--path <dir>]

The first form removes one tracked item; the second revokes a
device's encryption-recipient entry and re-encrypts every currently-
tracked visibility: private file. Both always require role: human.";

const HELP_PURGE: &str = "\
Usage: wkp purge <path> [--path <dir>]

Erases a path from git history entirely, wrapping the upstream
git-filter-repo tool (not vendored -- install it separately).";

const HELP_SYNC: &str = "\
Usage: wkp sync [--remote <name>] [--path <dir>]
   or: wkp sync status [--path <dir>]

Fetches and pushes this device's branch, converging with peer devices
through the merge driver. `sync status` reports unresolved conflicts
without changing anything.";

const HELP_BUNDLE: &str = "\
Usage: wkp bundle export <output-path> [--since <ref>] [--path <dir>]
   or: wkp bundle import <bundle-path> [--path <dir>]

`export` writes a git bundle for air-gapped sync (everything, or
everything since --since). `import` applies a bundle produced by
`bundle export` on another device.";

const HELP_RESOLVE_CONFLICTS: &str = "\
Usage: wkp resolve-conflicts [path]

Resolves modify/delete conflicts (a deletion always loses to a
modification) by keeping the modification and proposing the deletion
as an inbox/ item instead of silently dropping either side.";

const HELP_HUB: &str = "\
Usage: wkp hub register --hub-url <url> --tenant <slug> --ca-cert <path> [--path <dir>]

Registers this device with a wkp-hub (RFC 8628 device flow), issuing
an mTLS device certificate. --ca-cert is the hub's own CA root.";

const HELP_WKPD: &str = "\
Usage: wkp wkpd --principal <p> --signing-key-file <path> [--path <dir>]
                 [--remote <name>] [--socket-path <path>] [--debounce-ms <n>]

Watch-triggered sync daemon: debounced auto-commit of local changes,
incremental re-index, opportunistic sync. Binds a Unix domain socket
only, peer-UID verified on every connection -- Linux only.";

const HELP_HOOKS: &str = "\
Usage: wkp hooks --framework <name>

Prints text for a harness to apply -- never writes a file itself.
Frameworks: claude_code, codex, opencode, hermes, agents_md.";

/// The real body of `wkp`: every subcommand's argument parsing, dispatch,
/// and error handling. Returns the process exit code instead of calling
/// `std::process::exit` directly (every one of this function's ~70
/// former direct exit points now `return`s instead) so `main` below can
/// wrap this call once, record one usage-metrics sample covering the
/// *whole* invocation regardless of which path through here it took, and
/// exit with the same code an external caller would have seen either
/// way -- `std::process::exit` cannot be intercepted after the fact
/// (it skips destructors and any code following it), so returning a
/// value is the only way to guarantee the metrics write happens on
/// every path, not just the successful ones.
fn dispatch() -> i32 {
    // Dispatching on a CLI flag, not a security-sensitive use of argv.
    let mut args = std::env::args().skip(1); // nosemgrep: rust.lang.security.args.args
    let first = args.next();

    // `--sandbox <backend> <COMMAND> [ARGS]` is handled before the
    // ordinary subcommand dispatch below: it isn't a subcommand itself,
    // it's a modifier that re-execs whatever subcommand follows it
    // inside a container (`podman_sandbox::run`). Checked here, first,
    // since it must consume two tokens (the flag and its backend)
    // before the real subcommand name is even reached.
    if first.as_deref() == Some("--sandbox") {
        let backend = args.next();
        match backend.as_deref() {
            Some("podman") => {
                let remaining: Vec<String> = args.collect();
                if remaining.is_empty() {
                    eprintln!("wkp: --sandbox podman requires a command, e.g. `wkp --sandbox podman index`");
                    return 1;
                }
                match podman_sandbox::run(&remaining) {
                    Ok(code) => return code,
                    Err(msg) => {
                        eprintln!("wkp: --sandbox podman failed: {msg}");
                        return 1;
                    }
                }
            }
            Some(other) => {
                eprintln!("wkp: unknown --sandbox backend '{other}' (supported: podman)");
                return 1;
            }
            None => {
                eprintln!("wkp: --sandbox requires a backend, e.g. `wkp --sandbox podman index`");
                return 1;
            }
        }
    }

    let command = first;
    match command.as_deref() {
        None => {
            eprintln!("{USAGE}");
            return 1;
        }
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
        }
        Some("--version" | "-V") => {
            println!("wkp {}", env!("CARGO_PKG_VERSION"));
        }
        Some("init") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_INIT}");
                return 0;
            }
            let mut args = args.into_iter();
            // Unlike every other subcommand, `init` had no argument
            // parser at all before this guard -- any unrecognized flag
            // (including `--help`, handled above, but also a plain typo)
            // silently became the literal path argument, creating a
            // bogus nested store instead of erroring.
            let path = match args.next() {
                Some(p) if p.starts_with('-') => {
                    eprintln!("wkp: unrecognized argument: {p}");
                    return 1;
                }
                Some(p) => PathBuf::from(p),
                None => std::env::current_dir().expect("wkp: cannot read cwd"),
            };
            if let Some(extra) = args.next() {
                eprintln!("wkp: unrecognized argument: {extra}");
                return 1;
            }
            match init::run_init(&path) {
                Ok(()) => println!("wkp: initialized store at {}", path.display()),
                Err(msg) => {
                    eprintln!("wkp: init failed: {msg}");
                    return 1;
                }
            }
        }
        Some("import") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_IMPORT}");
                return 0;
            }
            let mut args = args.into_iter();
            // Same missing-guard bug as `init` above: an unrecognized
            // flag used to silently become the literal path argument.
            let path = match args.next() {
                Some(p) if p.starts_with('-') => {
                    eprintln!("wkp: unrecognized argument: {p}");
                    return 1;
                }
                Some(p) => PathBuf::from(p),
                None => std::env::current_dir().expect("wkp: cannot read cwd"),
            };
            if let Some(extra) = args.next() {
                eprintln!("wkp: unrecognized argument: {extra}");
                return 1;
            }
            let claude_home = import::claude_home_from_env();
            match import::run_import(&path, claude_home.as_deref()) {
                Ok(summary) => println!("{summary}"),
                Err(msg) => {
                    eprintln!("wkp: import failed: {msg}");
                    return 1;
                }
            }
        }
        Some("search") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_SEARCH}");
                return 0;
            }
            match search::parse_search_args(args.into_iter()) {
                Ok(opts) => match search::run_search(&opts) {
                    Ok(output) => println!("{output}"),
                    Err(msg) => {
                        eprintln!("wkp: search failed: {msg}");
                        return 1;
                    }
                },
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("context") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_CONTEXT}");
                return 0;
            }
            match search::parse_search_args(args.into_iter()) {
                Ok(opts) => match context::run_context(&opts) {
                    Ok(output) => println!("{output}"),
                    Err(msg) => {
                        eprintln!("wkp: context failed: {msg}");
                        return 1;
                    }
                },
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("traverse") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_TRAVERSE}");
                return 0;
            }
            match context::parse_traverse_args(args.into_iter()) {
                Ok(opts) => match context::run_traverse(&opts) {
                    Ok(output) => println!("{output}"),
                    Err(msg) => {
                        eprintln!("wkp: traverse failed: {msg}");
                        return 1;
                    }
                },
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("index") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_INDEX}");
                return 0;
            }
            match index_cmd::parse_index_args(args.into_iter()) {
                Ok(opts) => {
                    apply_write_sandbox(&opts.path);
                    match index_cmd::run_index_cli(&opts) {
                        Ok(summary) => println!("{summary}"),
                        Err(msg) => {
                            eprintln!("wkp: index failed: {msg}");
                            return 1;
                        }
                    }
                }
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("materialize") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_MATERIALIZE}");
                return 0;
            }
            match materialize::parse_materialize_args(args.into_iter()) {
                Ok(opts) => match materialize::run_materialize(&opts) {
                    Ok(()) => {}
                    Err(msg) => {
                        eprintln!("wkp: materialize failed: {msg}");
                        return 1;
                    }
                },
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("remember") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_REMEMBER}");
                return 0;
            }
            match remember::parse_remember_args(args.into_iter()) {
                Ok(opts) => {
                    apply_write_sandbox(&opts.path);
                    match remember::run_remember(&opts) {
                        Ok(summary) => println!("{summary}"),
                        Err(msg) => {
                            eprintln!("wkp: remember failed: {msg}");
                            return 1;
                        }
                    }
                }
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("promote") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_PROMOTE}");
                return 0;
            }
            match promote::parse_promote_args(args.into_iter()) {
                Ok(opts) => {
                    apply_write_sandbox(&opts.path);
                    match promote::run_promote(&opts) {
                        Ok(summary) => println!("{summary}"),
                        Err(msg) => {
                            eprintln!("wkp: promote failed: {msg}");
                            return 1;
                        }
                    }
                }
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("hub") => {
            let sub = args.next();
            if matches!(sub.as_deref(), Some("--help" | "-h")) {
                println!("{HELP_HUB}");
                return 0;
            }
            match sub.as_deref() {
                Some("register") => {
                    let args: Vec<String> = args.collect();
                    if wants_help(&args) {
                        println!("{HELP_HUB}");
                        return 0;
                    }
                    match hub_register::parse_hub_register_args(args.into_iter()) {
                        Ok(opts) => match hub_register::run_hub_register(&opts) {
                            Ok(summary) => println!("{summary}"),
                            Err(msg) => {
                                eprintln!("wkp: hub register failed: {msg}");
                                return 1;
                            }
                        },
                        Err(msg) => {
                            eprintln!("wkp: {msg}");
                            return 1;
                        }
                    }
                }
                _ => {
                    eprintln!(
                        "wkp: usage: wkp hub register --hub-url <url> --tenant <slug> \
                             --ca-cert <path> [--path <dir>]"
                    );
                    return 1;
                }
            }
        }
        Some("forget") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_FORGET}");
                return 0;
            }
            match forget::parse_forget_args(args.into_iter()) {
                Ok(opts) => {
                    apply_write_sandbox(&opts.path);
                    let result = match &opts.target {
                        forget::ForgetTarget::Item(item_path) => {
                            let item_path = item_path.clone();
                            forget::run_forget_item(&opts, &item_path).map(|s| s.to_string())
                        }
                        forget::ForgetTarget::Device(device_id) => {
                            let device_id = device_id.clone();
                            forget::run_forget_device(&opts, &device_id).map(|s| s.to_string())
                        }
                    };
                    match result {
                        Ok(summary) => println!("{summary}"),
                        Err(msg) => {
                            eprintln!("wkp: forget failed: {msg}");
                            return 1;
                        }
                    }
                }
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("purge") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_PURGE}");
                return 0;
            }
            match purge::parse_purge_args(args.into_iter()) {
                Ok(opts) => match purge::run_purge(&opts) {
                    Ok(summary) => println!("{summary}"),
                    Err(msg) => {
                        eprintln!("wkp: purge failed: {msg}");
                        return 1;
                    }
                },
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        // Deliberately no `ensure_min_git_version` check: `wkp hooks`
        // never touches git or the store, only prints static text (design
        // 3.3: "the binary never writes outside its own store" -- this
        // command doesn't write anywhere at all).
        Some("hooks") => {
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_HOOKS}");
                return 0;
            }
            match hooks::parse_hooks_args(args.into_iter()) {
                Ok(framework) => match hooks::render_hooks(&framework) {
                    Ok(text) => println!("{text}"),
                    Err(msg) => {
                        eprintln!("wkp: {msg}");
                        return 1;
                    }
                },
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        // Deliberately no `ensure_min_git_version` check: git itself
        // invokes this (per its own merge-driver protocol), not a human
        // typing `wkp`, and it only reads/writes the three plain temp
        // files git hands it -- no repo access, no `wkp-git` call at all.
        Some("merge-driver") => {
            let mut positional = args;
            let (Some(ancestor), Some(ours), Some(theirs)) =
                (positional.next(), positional.next(), positional.next())
            else {
                eprintln!(
                    "wkp: merge-driver requires three paths: <ancestor> <ours> <theirs> \
                     (git supplies these itself per its merge-driver protocol)"
                );
                return 1;
            };
            if let Err(msg) = merge_driver::run_merge_driver(
                Path::new(&ancestor),
                Path::new(&ours),
                Path::new(&theirs),
            ) {
                eprintln!("wkp: merge-driver failed: {msg}");
                return 1;
            }
        }
        // Deliberately no `ensure_min_git_version` check, same reasoning
        // as merge-driver above: git itself invokes this per its own
        // clean/smudge filter protocol (gitattributes(5)), with cwd
        // already set to the top of the working tree -- confirmed by
        // hand, not assumed, since that's exactly what
        // `filter::run_filter_clean`/`run_filter_smudge` rely on to find
        // the store's `recipients` file and `.wkp/device-identity`
        // fallback without a separate `--store` flag.
        Some("filter") => {
            use std::io::{Read, Write};
            let direction = args.next();
            let _file_path = args.next(); // %f -- accepted per git's protocol, not needed by content-based detection
            let store_root = std::env::current_dir().expect("wkp: cannot read cwd");

            let mut content = Vec::new();
            std::io::stdin()
                .read_to_end(&mut content)
                .expect("wkp: failed to read filter input from stdin");

            match direction.as_deref() {
                Some("clean") => match filter::run_filter_clean(&store_root, &content) {
                    Ok(output) => {
                        std::io::stdout()
                            .write_all(&output)
                            .expect("wkp: failed to write filter output");
                    }
                    Err(msg) => {
                        eprintln!("wkp: filter clean failed: {msg}");
                        return 1;
                    }
                },
                Some("smudge") => {
                    let output = filter::run_filter_smudge(&store_root, &content);
                    std::io::stdout()
                        .write_all(&output)
                        .expect("wkp: failed to write filter output");
                }
                _ => {
                    eprintln!("wkp: filter requires a direction: clean|smudge");
                    return 1;
                }
            }
        }
        Some("resolve-conflicts") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_RESOLVE_CONFLICTS}");
                return 0;
            }
            match resolve_conflicts::parse_resolve_conflicts_args(args.into_iter()) {
                Ok(path) => match resolve_conflicts::resolve_modify_delete_conflicts(&path) {
                    Ok(inbox_paths) if inbox_paths.is_empty() => {
                        println!("wkp: no modify/delete conflicts found");
                    }
                    Ok(inbox_paths) => {
                        for inbox_path in inbox_paths {
                            println!("wkp: kept modification, proposed deletion at {inbox_path}");
                        }
                    }
                    Err(msg) => {
                        eprintln!("wkp: resolve-conflicts failed: {msg}");
                        return 1;
                    }
                },
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        Some("sync") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            // `wkp sync status` is a sub-subcommand; anything else (or
            // nothing at all) falls through to the ordinary `wkp sync
            // [--remote <name>] [--path <dir>]` flow, so the peeked token
            // has to be put back for `parse_sync_args` when it isn't
            // "status".
            let mut args = args;
            let first = args.next();
            if matches!(first.as_deref(), Some("--help" | "-h")) {
                println!("{HELP_SYNC}");
                return 0;
            }
            if first.as_deref() == Some("status") {
                let args: Vec<String> = args.collect();
                if wants_help(&args) {
                    println!("{HELP_SYNC}");
                    return 0;
                }
                match sync_cmd::parse_sync_status_args(args.into_iter()) {
                    Ok(path) => match sync_cmd::run_sync_status(&path) {
                        Ok(conflicts) => {
                            println!("{}", sync_cmd::SyncStatusSummary { conflicts })
                        }
                        Err(msg) => {
                            eprintln!("wkp: sync status failed: {msg}");
                            return 1;
                        }
                    },
                    Err(msg) => {
                        eprintln!("wkp: {msg}");
                        return 1;
                    }
                }
            } else {
                let rebuilt: Vec<String> = first.into_iter().chain(args).collect();
                if wants_help(&rebuilt) {
                    println!("{HELP_SYNC}");
                    return 0;
                }
                match sync_cmd::parse_sync_args(rebuilt.into_iter()) {
                    Ok(opts) => match sync_cmd::run_sync(&opts) {
                        Ok(summary) => println!("{summary}"),
                        Err(msg) => {
                            eprintln!("wkp: sync failed: {msg}");
                            return 1;
                        }
                    },
                    Err(msg) => {
                        eprintln!("wkp: {msg}");
                        return 1;
                    }
                }
            }
        }
        Some("bundle") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let sub = args.next();
            if matches!(sub.as_deref(), Some("--help" | "-h")) {
                println!("{HELP_BUNDLE}");
                return 0;
            }
            match sub.as_deref() {
                Some("export") => {
                    let args: Vec<String> = args.collect();
                    if wants_help(&args) {
                        println!("{HELP_BUNDLE}");
                        return 0;
                    }
                    match bundle::parse_bundle_export_args(args.into_iter()) {
                        Ok(opts) => match bundle::run_bundle_export(&opts) {
                            Ok(summary) => println!("{summary}"),
                            Err(msg) => {
                                eprintln!("wkp: bundle export failed: {msg}");
                                return 1;
                            }
                        },
                        Err(msg) => {
                            eprintln!("wkp: {msg}");
                            return 1;
                        }
                    }
                }
                Some("import") => {
                    let args: Vec<String> = args.collect();
                    if wants_help(&args) {
                        println!("{HELP_BUNDLE}");
                        return 0;
                    }
                    match bundle::parse_bundle_import_args(args.into_iter()) {
                        Ok(opts) => match bundle::run_bundle_import(&opts) {
                            Ok(summary) => println!("{summary}"),
                            Err(msg) => {
                                eprintln!("wkp: bundle import failed: {msg}");
                                return 1;
                            }
                        },
                        Err(msg) => {
                            eprintln!("wkp: {msg}");
                            return 1;
                        }
                    }
                }
                other => {
                    eprintln!(
                        "wkp: bundle requires a subcommand: export or import (got {other:?})"
                    );
                    return 1;
                }
            }
        }
        Some("wkpd") => {
            if let Err(msg) = wkp_git::ensure_min_git_version() {
                eprintln!("{msg}");
                return 1;
            }
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_WKPD}");
                return 0;
            }
            match wkpd::parse_wkpd_args(args.into_iter()) {
                Ok(opts) => {
                    if let Err(msg) = wkpd::run_wkpd(&opts) {
                        eprintln!("wkp: wkpd failed: {msg}");
                        return 1;
                    }
                }
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        // Deliberately no `ensure_min_git_version` check, same reasoning
        // as `hooks` above: `wkp usage` only ever reads `.wkp/metrics.db`
        // directly via `wkp_core::usage`, never `wkp-git` plumbing.
        Some("usage") => {
            let args: Vec<String> = args.collect();
            if wants_help(&args) {
                println!("{HELP_USAGE}");
                return 0;
            }
            match usage_cmd::parse_usage_args(args.into_iter()) {
                Ok(opts) => match usage_cmd::run_usage(&opts) {
                    Ok(output) => println!("{output}"),
                    Err(msg) => {
                        eprintln!("wkp: usage failed: {msg}");
                        return 1;
                    }
                },
                Err(msg) => {
                    eprintln!("wkp: {msg}");
                    return 1;
                }
            }
        }
        // Undocumented on purpose (M6-1, issue #170): not real user
        // subcommands, just a way for `tests/sandbox_integration.rs`
        // to exercise `sandbox.rs`'s two functions as real subprocesses
        // of the actually-compiled `wkp` binary, using only safe
        // operations -- proving the *specific* denied syscalls
        // (`ptrace` etc.) actually fail needs an `unsafe` FFI call this
        // crate's own `#![forbid(unsafe_code)]` doesn't allow, so that
        // half was verified by hand instead; see `sandbox.rs`'s own
        // doc comments for exactly how.
        Some("__sandbox-self-test-write") => {
            let (Some(allowed), Some(denied)) = (args.next(), args.next()) else {
                eprintln!("wkp: usage: wkp __sandbox-self-test-write <allowed-dir> <denied-dir>");
                return 2;
            };
            if let Err(e) = sandbox::restrict_writes_to(&[Path::new(&allowed)]) {
                eprintln!("SANDBOX_SETUP_ERROR: {e}");
                return 2;
            }
            let inside_ok = std::fs::write(Path::new(&allowed).join("ok.txt"), b"ok").is_ok();
            let outside_denied =
                std::fs::write(Path::new(&denied).join("nope.txt"), b"nope").is_err();
            println!("INSIDE_WRITE_OK={inside_ok} OUTSIDE_WRITE_DENIED={outside_denied}");
            return if inside_ok && outside_denied { 0 } else { 1 };
        }
        Some("__sandbox-self-test-syscalls") => {
            // Takes a directory to probe into rather than reaching for
            // `std::env::temp_dir()` itself: a shared, world-writable
            // system temp directory with a predictable name is exactly
            // the "insecure temporary file" pattern our own CI's
            // semgrep rust ruleset flags (symlink-preexistence attacks
            // on a guessable path). The caller (`tests/sandbox_integration.rs`)
            // creates a fresh, exclusively-owned `tempfile::tempdir()`
            // for this -- the same secure-creation pattern
            // `__sandbox-self-test-write` above already relies on for
            // its own directories -- so this binary never has to make
            // its own claim about temp-file safety.
            let Some(probe_dir) = args.next() else {
                eprintln!("wkp: usage: wkp __sandbox-self-test-syscalls <writable-dir>");
                return 2;
            };
            if let Err(e) = sandbox::restrict_dangerous_syscalls() {
                eprintln!("SANDBOX_SETUP_ERROR: {e}");
                return 2;
            }
            // Only a *safe* probe is possible in this crate (see the
            // comment above `Some("__sandbox-self-test-write")`):
            // confirms an ordinary syscall still works after the
            // filter is applied, which alone would catch a
            // catastrophically backwards filter (one that denies
            // everything by default instead of the intended handful).
            let ordinary_write_ok =
                std::fs::write(Path::new(&probe_dir).join("ok.txt"), b"ok").is_ok();
            println!("ORDINARY_WRITE_OK={ordinary_write_ok}");
            return if ordinary_write_ok { 0 } else { 1 };
        }
        Some(unknown) => {
            eprintln!("wkp: unknown subcommand '{unknown}'\n");
            eprintln!("{USAGE}");
            return 1;
        }
    }

    // Reached only by an arm above that didn't `return` early -- every
    // such arm is a success path (`--help`/`--version`, or a subcommand
    // whose own `Ok(..)` branch just printed and fell through).
    0
}

/// `.wkp/metrics.db`'s counter name for one subcommand's exit status.
/// Two counters share one tool rather than widening the ring buffer's row
/// shape (design 5.5) to carry a success/failure split directly: `<tool>`
/// always gets one sample (every invocation, successful or not), and
/// `<tool>.err` gets a second sample *only* when the exit code was
/// nonzero. `wkp usage` (or any other reader) recovers the failure count
/// from `<tool>.err`'s own `n` and the success count as `<tool>`'s `n`
/// minus `<tool>.err`'s `n` -- no schema change needed if a future metric
/// needs the same treatment.
fn error_metric_name(tool: &str) -> String {
    format!("{tool}.err")
}

/// Best-effort: records one subcommand invocation's latency and exit
/// status into `.wkp/metrics.db`, on by default (design 5.5, ADR-0016).
/// Never touches anything if the current directory isn't already an
/// initialized store (`.wkp/` doesn't exist) -- this must never be the
/// reason a `.wkp/` directory gets created, only ever piggyback on one
/// that's already there. Never lets a metrics-write failure (a locked
/// file, a full disk, anything) affect the real command's own exit code
/// -- this is instrumentation, not a correctness-bearing subsystem.
///
/// `tool` is `argv[1]` verbatim (whatever `main` resolved before calling
/// `dispatch`), including flags like `--help`. The one thing intentionally
/// *not* handled here: `wkp --sandbox podman index` records under the
/// tool name `--sandbox`, not `index` -- the real subcommand is one level
/// deeper than `main` peeks, and resolving it accurately isn't worth the
/// extra complexity for what's still an approximate, best-effort signal.
fn record_invocation(tool: &str, elapsed: std::time::Duration, exit_code: i32) {
    if tool.is_empty() {
        return;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let wkp_dir = cwd.join(".wkp");
    if !wkp_dir.is_dir() {
        return;
    }
    let Ok(mut conn) = wkp_core::usage::open_usage_db(&wkp_dir.join("metrics.db")) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let _ = wkp_core::usage::record_counter(&mut conn, tool, elapsed, now);
    if exit_code != 0 {
        let _ = wkp_core::usage::record_counter(&mut conn, &error_metric_name(tool), elapsed, now);
    }
}

fn main() {
    let start = std::time::Instant::now();
    // A second, independent read of argv (not the one `dispatch` itself
    // consumes) purely to name the metric this invocation records --
    // `std::env::args()` can be called any number of times, each
    // yielding a fresh iterator over the same process arguments, so this
    // never disturbs `dispatch`'s own parsing. Naming a local metric, not
    // a security-sensitive use of argv -- same reasoning as `dispatch`'s
    // own identical suppression on its first read of argv.
    let tool = std::env::args().nth(1).unwrap_or_default(); // nosemgrep: rust.lang.security.args.args
    let code = dispatch();
    record_invocation(&tool, start.elapsed(), code);
    std::process::exit(code);
}

/// Writes `content` to `dest` via a temp file in the same directory,
/// renamed into place -- never in place (CLAUDE.md hard rule). Shared by
/// `wkp remember`/`wkp promote`/`wkp materialize`, the three write paths
/// that produce a file a harness or a human reads back.
fn atomic_write(dest: &Path, content: &str) -> Result<(), String> {
    let file_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "materialized.md".to_string());
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dest.with_file_name(format!(".{file_name}.tmp-{pid}-{nanos}"));
    let result = std::fs::write(&tmp, content).map_err(|e| e.to_string());
    match result {
        Ok(()) => std::fs::rename(&tmp, dest).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}
