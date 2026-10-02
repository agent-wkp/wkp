//! `wkp materialize`: writes `.wkp/tier{N}.md` (design 4.2, M1-6).

use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct MaterializeOptions {
    pub(crate) path: PathBuf,
    pub(crate) tier: u8,
    /// Feedback item 3 ("generate Tier 0 as a topic-to-path routing
    /// table"): when set, splices a `<wkp-routing-table>` block listing
    /// every tier-1 item's title and path into tier 0's own output.
    /// Only valid with `--tier 0` -- see `parse_materialize_args`.
    pub(crate) routing_table: bool,
}

/// Parses `wkp materialize --tier N [--routing-table] [--path DIR]`.
/// `--tier` is required (no default): materializing "whatever tier" by
/// accident is exactly the kind of silent behavior CLAUDE.md's "no
/// shortcuts" list warns against for tier promotion. `--routing-table`
/// is rejected outright for any tier but `0` -- same reasoning, applied
/// to a second flag: silently ignoring it on `--tier 1`/`--tier 2`
/// would be a shortcut that looks like it worked.
pub(crate) fn parse_materialize_args(
    mut args: impl Iterator<Item = String>,
) -> Result<MaterializeOptions, String> {
    let mut path = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut tier: Option<u8> = None;
    let mut routing_table = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tier" => {
                let v = args.next().ok_or("--tier requires a value")?;
                tier = Some(
                    v.parse::<u8>()
                        .map_err(|_| format!("invalid --tier value: {v}"))?,
                );
            }
            "--routing-table" => routing_table = true,
            "--path" => {
                let v = args.next().ok_or("--path requires a value")?;
                path = PathBuf::from(v);
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    let tier = tier.ok_or_else(|| {
        "materialize requires --tier N, e.g. `wkp materialize --tier 0`".to_string()
    })?;
    if routing_table && tier != 0 {
        return Err(
            "--routing-table only applies to --tier 0 (it splices a routing block into tier \
             0's own output; there is nothing for it to splice into at any other tier)"
                .to_string(),
        );
    }
    Ok(MaterializeOptions {
        path,
        tier,
        routing_table,
    })
}

/// `wkp materialize --tier N`: writes `.wkp/tier{N}.md` (design 4.2, M1-6),
/// atomically -- a temp file in the same directory, renamed into place
/// (CLAUDE.md hard rule: `tier0.md` is a file a harness reads, same
/// atomicity requirement as `index.db`). Design 4.3's session-start
/// injection target (`cat tier0.md` < 1ms/2ms) is trivially met once this
/// file exists: it's a plain file read, no `wkp` code runs on that path at
/// all.
///
/// `--routing-table` (feedback item 3) splices a `<wkp-routing-table>`
/// block before the closing `</wkp-context>` tag, built from
/// `wkp_core::index::routing_table(&conn, 1)` -- tier 1 specifically,
/// never tier 2: see that function's own doc comment for why using
/// anything but `1` here would defeat the human-signed-commit guarantee
/// tier 0 content already carries.
pub(crate) fn run_materialize(opts: &MaterializeOptions) -> Result<(), String> {
    let index_path = opts.path.join(".wkp/index.db");
    let conn = wkp_core::index::open_index(&index_path).map_err(|e| e.to_string())?;
    let mut content = wkp_core::index::materialize(&conn, opts.tier).map_err(|e| e.to_string())?;
    if opts.routing_table {
        let table = wkp_core::index::routing_table(&conn, 1).map_err(|e| e.to_string())?;
        let block = if table.is_empty() {
            "<wkp-routing-table>\n(tier 1 is empty -- nothing to route to yet)\n</wkp-routing-table>\n\n".to_string()
        } else {
            format!("<wkp-routing-table>\n{table}</wkp-routing-table>\n\n")
        };
        if let Some(pos) = content.rfind("</wkp-context>") {
            content.insert_str(pos, &block);
        }
    }
    let dest = opts.path.join(format!(".wkp/tier{}.md", opts.tier));
    crate::atomic_write(&dest, &content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{args, generate_test_key_and_register, temp_dir, test_init};

    #[test]
    fn parse_materialize_args_requires_tier() {
        assert!(parse_materialize_args(args(&[])).is_err());
    }

    #[test]
    fn parse_materialize_args_reads_tier_and_path() {
        let opts = parse_materialize_args(args(&["--tier", "1", "--path", "/tmp/x"]))
            .expect("parse_materialize_args");
        assert_eq!(opts.tier, 1);
        assert_eq!(opts.path, PathBuf::from("/tmp/x"));
        assert!(!opts.routing_table);
    }

    #[test]
    fn parse_materialize_args_reads_routing_table_flag() {
        let opts = parse_materialize_args(args(&["--tier", "0", "--routing-table"]))
            .expect("parse_materialize_args");
        assert!(opts.routing_table);
    }

    #[test]
    fn parse_materialize_args_rejects_routing_table_on_a_non_zero_tier() {
        let err = parse_materialize_args(args(&["--tier", "1", "--routing-table"]))
            .expect_err("--routing-table should be rejected on --tier 1");
        assert!(err.contains("--routing-table"), "got: {err}");
        assert!(err.contains("--tier 0"), "got: {err}");
    }

    #[test]
    fn run_materialize_writes_tier0_and_tier1_content() {
        let temp = temp_dir("materialize");
        let dir = temp.path();
        test_init(dir).expect("run_init");
        std::fs::write(
            dir.join("a.md"),
            "---\ntitle: A\ntype: project-state\n---\n\ntier zero body\n",
        )
        .expect("write a.md");
        std::fs::write(
            dir.join("b.md"),
            "---\ntitle: B\ntype: feedback\n---\n\ntier one body\n",
        )
        .expect("write b.md");
        // M2-6: tier 0/1 now requires a human-signed commit, not just
        // the right `type:` -- an unsigned `commit_all` would leave
        // both items at tier 2 regardless of frontmatter.
        let key = generate_test_key_and_register(dir, "human:alice");
        wkp_git::signed_commit::signed_commit(
            dir,
            &[PathBuf::from("a.md"), PathBuf::from("b.md")],
            "seed",
            "human:alice",
            &key.private_path,
            &wkp_git::provenance::Provenance::default(),
        )
        .expect("signed_commit");
        crate::index_cmd::run_index(dir).expect("run_index");

        run_materialize(&MaterializeOptions {
            path: dir.to_path_buf(),
            tier: 0,
            routing_table: false,
        })
        .expect("materialize tier 0");
        let tier0 = std::fs::read_to_string(dir.join(".wkp/tier0.md")).expect("read tier0.md");
        assert!(tier0.starts_with("<wkp-context tier=\"0\">"));
        assert!(tier0.contains("tier zero body"));
        assert!(!tier0.contains("tier one body"));

        run_materialize(&MaterializeOptions {
            path: dir.to_path_buf(),
            tier: 1,
            routing_table: false,
        })
        .expect("materialize tier 1");
        let tier1 = std::fs::read_to_string(dir.join(".wkp/tier1.md")).expect("read tier1.md");
        assert!(tier1.contains("tier one body"));
        assert!(!tier1.contains("tier zero body"));
    }

    #[test]
    fn run_materialize_with_routing_table_splices_tier1_titles_and_paths_into_tier0() {
        let temp = temp_dir("materialize-routing-table");
        let dir = temp.path();
        test_init(dir).expect("run_init");
        std::fs::write(
            dir.join("a.md"),
            "---\ntitle: A\ntype: project-state\n---\n\ntier zero body\n",
        )
        .expect("write a.md");
        std::fs::write(
            dir.join("b.md"),
            "---\ntitle: Routing Critical Note\ntype: feedback\n---\n\ntier one body\n",
        )
        .expect("write b.md");
        let key = generate_test_key_and_register(dir, "human:alice");
        wkp_git::signed_commit::signed_commit(
            dir,
            &[PathBuf::from("a.md"), PathBuf::from("b.md")],
            "seed",
            "human:alice",
            &key.private_path,
            &wkp_git::provenance::Provenance::default(),
        )
        .expect("signed_commit");
        crate::index_cmd::run_index(dir).expect("run_index");

        run_materialize(&MaterializeOptions {
            path: dir.to_path_buf(),
            tier: 0,
            routing_table: true,
        })
        .expect("materialize tier 0 with routing table");
        let tier0 = std::fs::read_to_string(dir.join(".wkp/tier0.md")).expect("read tier0.md");
        assert!(tier0.contains("tier zero body"), "got: {tier0}");
        assert!(tier0.contains("<wkp-routing-table>"), "got: {tier0}");
        assert!(
            tier0.contains("Routing Critical Note — b.md"),
            "got: {tier0}"
        );
        // The routing table must stay inside the existing wrapper, not
        // appended after it -- a harness recognizing content only up to
        // `</wkp-context>` (AGENTS.md's own documented marker) must still
        // see it.
        assert!(tier0.ends_with("</wkp-context>\n"), "got: {tier0}");
    }

    #[test]
    fn run_materialize_with_routing_table_is_honest_when_tier1_is_empty() {
        let temp = temp_dir("materialize-routing-table-empty");
        let dir = temp.path();
        test_init(dir).expect("run_init");
        run_materialize(&MaterializeOptions {
            path: dir.to_path_buf(),
            tier: 0,
            routing_table: true,
        })
        .expect("materialize tier 0 with routing table");
        let tier0 = std::fs::read_to_string(dir.join(".wkp/tier0.md")).expect("read tier0.md");
        assert!(tier0.contains("tier 1 is empty"), "got: {tier0}");
    }

    #[test]
    fn run_materialize_never_includes_inbox_items() {
        let temp = temp_dir("materialize-inbox");
        let dir = temp.path();
        test_init(dir).expect("run_init");
        std::fs::create_dir_all(dir.join("inbox/import")).expect("create inbox dir");
        std::fs::write(
            dir.join("inbox/import/claude-md.md"),
            "---\ntitle: Imported\ntype: project-state\nconfidence: proposed\n---\n\nshould never be auto-injected\n",
        )
        .expect("write inbox item");
        wkp_git::commit_all(dir, "seed").expect("commit_all");
        crate::index_cmd::run_index(dir).expect("run_index");

        run_materialize(&MaterializeOptions {
            path: dir.to_path_buf(),
            tier: 0,
            routing_table: false,
        })
        .expect("materialize tier 0");
        let tier0 = std::fs::read_to_string(dir.join(".wkp/tier0.md")).expect("read tier0.md");
        assert!(!tier0.contains("should never be auto-injected"));
    }

    #[test]
    fn run_materialize_is_atomic_on_a_failed_write() {
        let temp = temp_dir("materialize-atomic");
        let dir = temp.path();
        test_init(dir).expect("run_init");
        run_materialize(&MaterializeOptions {
            path: dir.to_path_buf(),
            tier: 0,
            routing_table: false,
        })
        .expect("initial materialize");
        let original = std::fs::read(dir.join(".wkp/tier0.md")).expect("read original tier0.md");

        // Force the temp-file write to fail: make the .wkp/ directory
        // read-only so `atomic_write`'s initial `fs::write` to a new temp
        // path inside it cannot succeed. Restore the original mode
        // explicitly afterward rather than `set_readonly(false)`, which on
        // Unix would leave the directory world-writable (0o777).
        use std::os::unix::fs::PermissionsExt;
        let wkp_dir = dir.join(".wkp");
        let original_mode = std::fs::metadata(&wkp_dir).unwrap().permissions().mode();
        std::fs::set_permissions(&wkp_dir, std::fs::Permissions::from_mode(0o555))
            .expect("make .wkp/ read-only");

        let result = run_materialize(&MaterializeOptions {
            path: dir.to_path_buf(),
            tier: 0,
            routing_table: false,
        });

        std::fs::set_permissions(&wkp_dir, std::fs::Permissions::from_mode(original_mode))
            .expect("restore .wkp/ original permissions");

        assert!(
            result.is_err(),
            "expected the write to a read-only dir to fail"
        );
        let after = std::fs::read(dir.join(".wkp/tier0.md")).expect("read tier0.md after failure");
        assert_eq!(
            original, after,
            "a failed materialize must not touch the existing tier0.md"
        );
    }
}
