#![no_main]

use libfuzzer_sys::fuzz_target;
use wkp_core::frontmatter::Frontmatter;
use wkp_core::index::{build_in_memory, search, Item, SearchFilter};

/// Regression coverage for a real bug found by hand, not by this fuzzer
/// (William, 2026-09): `wkp search "RHOAI 3.6 release dates"` raised a
/// raw `fts5: syntax error near "."` instead of returning results,
/// because the query string was bound directly to `items MATCH ?` and
/// FTS5 re-parses a bound MATCH parameter's *value* with its own query
/// grammar (barewords, `AND`/`OR`/`NOT`, phrase quotes, `column:`
/// filters, `*` prefixes) -- `.` is not a valid bareword character
/// there. `search::sanitize_fts_query` (private to `wkp-core`) now
/// quotes every term before binding it, specifically so that no user
/// query, however it's punctuated, can ever reach FTS5's parser as
/// anything but a literal phrase.
///
/// This is exactly the class of check `docs/design/wkp-hub-design-v0.1.md`
/// already listed as an aspirational, not-yet-built target ("FTS5 query
/// construction" fuzzing, alongside merge-driver/age-filter/webhook
/// fuzzing) -- the invariant under test is "no query string, no matter
/// how adversarial, ever makes `search` return `Err` due to malformed
/// FTS5 syntax it generated internally." A caller error (a bad `--tier`
/// value, for instance) is a different code path entirely and not what
/// this target exercises -- `search` here never receives one, since the
/// filter is fixed and only `query` varies.
fuzz_target!(|data: &[u8]| {
    let query = String::from_utf8_lossy(data);

    let items = vec![Item {
        path: "a.md".to_string(),
        frontmatter: Frontmatter::default(),
        body: "RHOAI 3.6 release dates, UBI8 base image, AND OR NOT".to_string(),
        embedding: None,
        human_signed: false,
    }];
    let conn = build_in_memory(&items).expect("build a fixed in-memory index");

    match search(&conn, &query, &SearchFilter::default()) {
        Ok(_) => {}
        Err(e) => panic!(
            "search() must never fail on arbitrary user input, got {e:?} for query {query:?}"
        ),
    }
});
