//! Case folding follows the collation of the folded text, as PostgreSQL's
//! does.
//!
//! PostgreSQL folds by the `LC_CTYPE` of the text's collation, which for the
//! database default under a UTF-8 locale covers the whole of Unicode, while
//! SQLite's `lower` and `upper` fold ASCII only. A fold over anything but
//! ASCII text or a text collated `C` or `POSIX` therefore calls the functions
//! `with_case_fold_functions` names, and is refused without them. Every other
//! collation is refused outright. Every PostgreSQL answer named below was read
//! off PostgreSQL 17 under `en_US.utf8`.

use pg2sqlite::prelude::{CaseFoldFunctions, Pg2Sqlite, Pg2SqliteOptions};

use crate::helpers::{self, case_fold_functions, run_translated::run_translated_with};

/// The last statement of a translation under `options`, which is the probe.
fn probe(pg: &str, options: &Pg2SqliteOptions) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate_to_sql(options)
        .expect("translate")
        .into_vec()
        .pop()
        .expect("a probe statement")
}

/// The refusal `pg` earns without fold functions.
fn refusal(pg: &str) -> String {
    Pg2Sqlite::default()
        .sql(pg)
        .expect("parse")
        .translate(&Pg2SqliteOptions::default())
        .expect_err("a fold by the server's LC_CTYPE must refuse without fold functions")
        .to_string()
}

/// The first column of each row `pg` answers on a replica registering the
/// harness's fold functions.
fn answer(pg: &str) -> Vec<Option<String>> {
    run_translated_with(pg, &case_fold_functions::with_functions(Pg2SqliteOptions::default()))
}

/// The shape the README shows, on ASCII literals, which every collation folds
/// alike. Both halves are lowered by SQLite's builtin and the escape is the one
/// PostgreSQL applies by default.
#[test]
fn an_ascii_literal_ilike_folds_through_the_builtin() {
    assert_eq!(
        probe("SELECT 'ABC' ILIKE 'abc';", &Pg2SqliteOptions::default()),
        "SELECT lower('ABC') LIKE lower('abc') ESCAPE '\\'"
    );
    assert_eq!(
        probe("SELECT lower('ABC'), upper('abc');", &Pg2SqliteOptions::default()),
        "SELECT lower('ABC'), upper('abc')"
    );
}

/// PostgreSQL answers `ä@example.com` for the first, so the builtin's
/// `Ä@example.com` is refused rather than emitted, naming the call and the
/// option.
#[test]
fn a_fold_by_the_server_locale_is_refused_without_fold_functions() {
    for (pg, call) in [
        ("SELECT lower('Ä@example.com');", "lower('Ä@example.com')"),
        ("SELECT upper('äbc');", "upper('äbc')"),
        ("SELECT 'ÄBC' ILIKE 'äbc';", "'ÄBC' ILIKE 'äbc'"),
        ("CREATE TABLE t (email text); SELECT lower(email) FROM t;", "lower(email)"),
        ("CREATE TABLE t (email text); SELECT upper($1) FROM t;", "upper($1)"),
    ] {
        let error = refusal(pg);
        assert!(error.contains("with_case_fold_functions"), "{pg}: {error}");
        assert!(error.contains(call), "{pg}: {error}");
    }
}

/// The registered folds answer what PostgreSQL answers under glibc.
#[test]
fn fold_functions_answer_as_the_server_does() {
    assert_eq!(answer("SELECT lower('ÄBC');"), vec![Some("äbc".to_string())]);
    assert_eq!(answer("SELECT upper('äbc');"), vec![Some("ÄBC".to_string())]);
    assert_eq!(answer("SELECT lower('ΣΙΓΜΑ');"), vec![Some("σιγμα".to_string())]);
    assert_eq!(answer("SELECT upper('straße');"), vec![Some("STRAßE".to_string())]);
    assert_eq!(answer("SELECT lower('İSTANBUL');"), vec![Some("istanbul".to_string())]);
    assert_eq!(answer("SELECT upper('ᾀᾇᾐᾗᾠᾧᾳῃῳ');"), vec![Some("ᾈᾏᾘᾟᾨᾯᾼῌῼ".to_string())]);
    assert_eq!(answer("SELECT 'ÄBC' ILIKE 'äbc';"), vec![Some("1".to_string())]);
}

/// The check PostgreSQL refuses with 23514 for `Ä@example.com` refuses on the
/// replica too, and the folded address passes on both.
#[test]
fn a_check_comparing_with_lower_refuses_what_the_server_refuses() {
    let schema = "CREATE TABLE invitations (email text NOT NULL CHECK (email = lower(email)));";
    let options = case_fold_functions::with_functions(Pg2SqliteOptions::default());
    helpers::apply_with_options(
        &format!("{schema} INSERT INTO invitations VALUES ('ä@example.com');"),
        &options,
    )
    .expect("the folded address passes the check");
    let refused = helpers::apply_with_options(
        &format!("{schema} INSERT INTO invitations VALUES ('Ä@example.com');"),
        &options,
    )
    .err()
    .expect("an address with an upper case letter fails the check");
    assert!(refused.to_string().contains("CHECK constraint failed"), "{refused}");
}

/// A text collated `C` folds ASCII only on both engines, so it keeps the
/// builtin without fold functions, and with them too.
#[test]
fn a_c_collated_text_keeps_the_builtin() {
    let functions = case_fold_functions::with_functions(Pg2SqliteOptions::default());
    for options in [Pg2SqliteOptions::default(), functions] {
        assert_eq!(
            probe("CREATE TABLE t (code text COLLATE \"C\"); SELECT lower(code) FROM t;", &options),
            "SELECT lower(code) FROM t"
        );
        assert_eq!(
            probe(
                "CREATE TABLE t (email text); SELECT upper(email COLLATE \"POSIX\") FROM t;",
                &options
            ),
            "SELECT upper(email COLLATE BINARY) FROM t"
        );
    }
}

/// A column of a function called in `FROM` folds by the collation of the
/// arguments the call writes, as PostgreSQL derives it, so a `C` argument
/// keeps the builtin and a default one needs the fold functions.
#[test]
fn a_function_result_column_folds_as_its_arguments() {
    let echo = "CREATE FUNCTION echo(t text) RETURNS SETOF text LANGUAGE sql AS $$ SELECT t $$;";
    let options = Pg2SqliteOptions::default().with_user_defined_functions(["echo"]);
    let collated =
        probe(&format!("{echo} SELECT upper(e.v) FROM echo('a' COLLATE \"C\") AS e(v);"), &options);
    assert!(collated.contains("upper(e.v)"), "{collated}");
    let default = format!("{echo} SELECT upper(e.v) FROM echo('a') AS e(v);");
    let error = Pg2Sqlite::default()
        .sql(&default)
        .expect("parse")
        .translate(&options)
        .expect_err("a default-collated argument folds by the server's LC_CTYPE")
        .to_string();
    assert!(error.contains("upper(e.v)"), "{error}");
    let registered = probe(&default, &case_fold_functions::with_functions(options));
    assert!(registered.contains("pg_upper(e.v)"), "{registered}");
}

/// A collation such as `tr-TR-x-icu` folds `I` to `ı`, which the functions
/// folding by the server's `LC_CTYPE` do not answer, so it stays refused with
/// them set.
#[test]
fn another_collation_stays_refused_with_fold_functions() {
    let options = case_fold_functions::with_functions(Pg2SqliteOptions::default());
    for pg in [
        "CREATE TABLE t (name text COLLATE \"tr-TR-x-icu\"); SELECT lower(name) FROM t;",
        "CREATE TABLE t (name text); SELECT upper(name COLLATE \"tr-TR-x-icu\") FROM t;",
    ] {
        let error = Pg2Sqlite::default()
            .sql(pg)
            .expect("parse")
            .translate(&options)
            .expect_err("a fold by another collation's locale must refuse")
            .to_string();
        assert!(error.contains("is not a valid SQLite collation"), "{pg}: {error}");
    }
}

/// A server whose `LC_CTYPE` is `C` declares SQLite's own functions, and
/// every fold then goes through them.
#[test]
fn an_ascii_server_names_the_builtins() {
    let options = Pg2SqliteOptions::default()
        .with_case_fold_functions(CaseFoldFunctions::new("lower", "upper"));
    assert_eq!(
        probe("CREATE TABLE t (email text); SELECT lower(email) FROM t;", &options),
        "SELECT lower(email) FROM t"
    );
}

/// A plain `LIKE` stays case-sensitive, which is what the emitted
/// `PRAGMA case_sensitive_like` is for and what PostgreSQL does.
#[test]
fn a_plain_like_is_case_sensitive_on_both_engines() {
    assert_eq!(
        run_translated_with("SELECT 'ABC' LIKE 'abc';", &Pg2SqliteOptions::default()),
        vec![Some("0".to_string())]
    );
}
