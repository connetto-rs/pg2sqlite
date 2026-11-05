//! Build/dependency hygiene checks.
//!
//! pg2sqlite and sql-traits both name sqlparser in their own manifests, and
//! the workspace patch routes both onto one source. The invariant is that
//! exactly one sqlparser version ends up in the dependency graph, so AST types
//! unify between this crate and sql-traits.

#[test]
fn lockfile_has_single_sqlparser_version() {
    let cargo_lock = include_str!("../../Cargo.lock");
    let sqlparser_versions = cargo_lock
        .split("\n\n")
        .filter(|chunk| chunk.contains("name = \"sqlparser\""))
        .filter_map(|chunk| {
            chunk.lines().find(|line| line.starts_with("version = ")).map(str::to_string)
        })
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        sqlparser_versions.len(),
        1,
        "Cargo.lock should contain exactly one sqlparser version so AST types \
         unify between pg2sqlite and sql-traits (found: {sqlparser_versions:?})"
    );
}
