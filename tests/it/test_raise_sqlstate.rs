//! What a trigger's `RAISE EXCEPTION` cannot carry to the replica: an option
//! besides `ERRCODE` and `MESSAGE` is dropped with a warning, and an `ERRCODE`
//! the replica cannot resolve when it translates the trigger is refused.

use pg2sqlite::warnings::TranslationWarning;

use crate::helpers::translate_with_default_warnings;

fn schema(raise: &str) -> String {
    format!(
        "CREATE TABLE readings (id INT PRIMARY KEY, val INT);
         CREATE FUNCTION reject_reading() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             IF NEW.val < 0 THEN
                 {raise};
             END IF;
             RETURN NEW;
         END; $$;
         CREATE TRIGGER guard BEFORE INSERT ON readings FOR EACH ROW
             EXECUTE FUNCTION reject_reading();"
    )
}

fn refusal(raise: &str) -> String {
    translate_with_default_warnings(&schema(raise)).expect_err("the RAISE is refused").to_string()
}

#[test]
fn a_hint_and_a_detail_are_dropped_with_a_warning_each() {
    let (_, warnings) = translate_with_default_warnings(&schema(
        "RAISE EXCEPTION 'negative' USING ERRCODE = 'check_violation', HINT = 'h', DETAIL = 'd'",
    ))
    .expect("the RAISE translates");
    let dropped: Vec<&str> = warnings
        .iter()
        .filter_map(|warning| {
            match warning {
                TranslationWarning::LossyDrop { construct, .. } => Some(construct.as_str()),
                _ => None,
            }
        })
        .collect();
    assert_eq!(dropped, ["RAISE USING HINT", "RAISE USING DETAIL"]);
}

#[test]
fn an_option_written_twice_is_refused() {
    let message = refusal("RAISE EXCEPTION 'twice' USING ERRCODE = '23514', ERRCODE = '42501'");
    assert!(message.contains("RAISE option already specified: ERRCODE"), "{message}");
}

#[test]
fn an_errcode_beside_a_condition_is_refused() {
    let message = refusal("RAISE check_violation USING ERRCODE = '42501'");
    assert!(message.contains("RAISE option already specified: ERRCODE"), "{message}");
}

#[test]
fn a_message_beside_a_format_is_refused() {
    let message = refusal("RAISE EXCEPTION 'format' USING MESSAGE = 'option'");
    assert!(message.contains("RAISE option already specified: MESSAGE"), "{message}");
}

#[test]
fn an_unknown_errcode_is_refused() {
    let message = refusal("RAISE EXCEPTION 'unknown' USING ERRCODE = 'Check_Violation'");
    assert!(message.contains("unrecognized exception condition \"Check_Violation\""), "{message}");
}

#[test]
fn a_computed_errcode_is_refused() {
    let message = refusal("RAISE EXCEPTION 'computed' USING ERRCODE = 'check_' || 'violation'");
    assert!(message.contains("only a string literal translates"), "{message}");
}
