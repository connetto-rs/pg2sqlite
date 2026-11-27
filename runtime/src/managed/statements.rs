//! The top-level statements of SQLite text, split where `sqlite3_complete`
//! ends one, with each transaction control recognised by its parse.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use sqlparser::{
    ast::{Statement, TransactionModifier},
    dialect::SQLiteDialect,
    keywords::Keyword,
    parser::Parser,
    tokenizer::{Token, TokenWithSpan, Tokenizer},
};

use crate::group::{BeginMode, TransactionControl};

/// One top-level statement of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceStatement<'sql> {
    /// A statement SQLite runs as written.
    Plain(&'sql str),
    /// A transaction control.
    Control(TransactionControl),
}

/// The way SQLite text fails to split into statements.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StatementError {
    /// The text does not tokenize.
    #[error("the SQL does not tokenize: {0}")]
    Tokenize(String),
    /// A `CREATE TRIGGER` body lacks its closing `END;`.
    #[error("the SQL ends inside a trigger body")]
    UnterminatedTrigger,
    /// A transaction control does not parse.
    #[error("the transaction control does not parse: {0}")]
    Control(String),
    /// A transaction control SQLite has no counterpart for.
    #[error("the transaction control `{0}` is not supported")]
    UnsupportedControl(String),
    /// A method running one statement received several.
    #[error("expected one statement, found {0}")]
    StatementCount(usize),
}

/// The token classes `sqlite3_complete` reads.
#[derive(Clone, Copy)]
enum Class {
    Semicolon,
    Space,
    Other,
    Explain,
    Create,
    Temporary,
    Trigger,
    End,
}

/// `sqlite3_complete`'s transitions, rows by state and columns by [`Class`].
/// State 1 is the start of a statement, and a semicolon reaching it ends one.
const TRANSITIONS: [[u8; 8]; 8] = [
    [1, 0, 2, 3, 4, 2, 2, 2],
    [1, 1, 2, 3, 4, 2, 2, 2],
    [1, 2, 2, 2, 2, 2, 2, 2],
    [1, 3, 3, 2, 4, 2, 2, 2],
    [1, 4, 2, 2, 2, 4, 5, 2],
    [6, 5, 5, 5, 5, 5, 5, 5],
    [6, 6, 5, 5, 5, 5, 5, 7],
    [1, 7, 5, 5, 5, 5, 5, 5],
];

const START: u8 = 1;
/// Inside a trigger body, outside its last statement's semicolon.
const TRIGGER: u8 = 5;
/// After a semicolon inside a trigger body, before its `END`.
const TRIGGER_SEMICOLON: u8 = 6;

fn class(token: &Token) -> Class {
    match token {
        Token::SemiColon => Class::Semicolon,
        Token::Whitespace(_) => Class::Space,
        Token::Word(word) => {
            match word.keyword {
                _ if word.quote_style.is_some() => Class::Other,
                Keyword::EXPLAIN => Class::Explain,
                Keyword::CREATE => Class::Create,
                Keyword::TEMP | Keyword::TEMPORARY => Class::Temporary,
                Keyword::TRIGGER => Class::Trigger,
                Keyword::END => Class::End,
                _ => Class::Other,
            }
        }
        _ => Class::Other,
    }
}

/// Splits `sql` into its top-level statements, dropping empty ones.
pub(crate) fn split(sql: &str) -> Result<Vec<SourceStatement<'_>>, StatementError> {
    let tokens = Tokenizer::new(&SQLiteDialect {}, sql)
        .tokenize_with_location()
        .map_err(|error| StatementError::Tokenize(error.to_string()))?;
    let offsets = Offsets::new(sql);
    let mut statements = Vec::new();
    let mut state = START;
    let mut first: Option<&TokenWithSpan> = None;
    let mut start = 0;
    for token in &tokens {
        let class = class(&token.token);
        if first.is_none() && !matches!(class, Class::Space | Class::Semicolon) {
            first = Some(token);
            start = offsets.byte(token.span.start.line, token.span.start.column);
        }
        state = TRANSITIONS[usize::from(state)][class as usize];
        if state == START
            && matches!(class, Class::Semicolon)
            && let Some(leading) = first.take()
        {
            let end = offsets.byte(token.span.start.line, token.span.start.column);
            statements.push(statement(leading, &sql[start..end])?);
        }
    }
    if state == TRIGGER || state == TRIGGER_SEMICOLON {
        return Err(StatementError::UnterminatedTrigger);
    }
    if let Some(leading) = first {
        statements.push(statement(leading, sql[start..].trim_end())?);
    }
    Ok(statements)
}

/// Splits `sql`, which must hold exactly one statement.
pub(crate) fn single(sql: &str) -> Result<SourceStatement<'_>, StatementError> {
    let mut statements = split(sql)?;
    match statements.len() {
        1 => Ok(statements.remove(0)),
        count => Err(StatementError::StatementCount(count)),
    }
}

fn statement<'sql>(
    leading: &TokenWithSpan,
    text: &'sql str,
) -> Result<SourceStatement<'sql>, StatementError> {
    let Token::Word(word) = &leading.token else {
        return Ok(SourceStatement::Plain(text));
    };
    if word.quote_style.is_some()
        || !matches!(
            word.keyword,
            Keyword::BEGIN
                | Keyword::COMMIT
                | Keyword::END
                | Keyword::ROLLBACK
                | Keyword::SAVEPOINT
                | Keyword::RELEASE
        )
    {
        return Ok(SourceStatement::Plain(text));
    }
    let parsed = Parser::new(&SQLiteDialect {})
        .try_with_sql(text)
        .and_then(|mut parser| parser.parse_statement())
        .map_err(|error| StatementError::Control(error.to_string()))?;
    let unsupported = || StatementError::UnsupportedControl(String::from(text));
    let control = match parsed {
        Statement::StartTransaction { modes, modifier, .. } if modes.is_empty() => {
            TransactionControl::Begin(match modifier {
                None | Some(TransactionModifier::Deferred) => BeginMode::Deferred,
                Some(TransactionModifier::Immediate) => BeginMode::Immediate,
                Some(TransactionModifier::Exclusive) => BeginMode::Exclusive,
                Some(TransactionModifier::Try | TransactionModifier::Catch) => {
                    return Err(unsupported());
                }
            })
        }
        Statement::Commit { chain: false, modifier: None, .. } => {
            TransactionControl::Commit { chain: false }
        }
        Statement::Rollback { chain: false, savepoint: None } => {
            TransactionControl::Rollback { chain: false }
        }
        Statement::Rollback { chain: false, savepoint: Some(name) } => {
            TransactionControl::RollbackTo(name.value)
        }
        Statement::Savepoint { name } => TransactionControl::Savepoint(name.value),
        Statement::ReleaseSavepoint { name } => TransactionControl::Release(name.value),
        _ => return Err(unsupported()),
    };
    Ok(SourceStatement::Control(control))
}

/// The byte offset of each line start, which turns the tokenizer's
/// one-based line and character column into a byte offset.
struct Offsets<'sql> {
    sql: &'sql str,
    lines: Vec<usize>,
}

impl<'sql> Offsets<'sql> {
    fn new(sql: &'sql str) -> Self {
        let lines = core::iter::once(0)
            .chain(sql.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Self { sql, lines }
    }

    fn byte(&self, line: u64, column: u64) -> usize {
        let line_start = usize::try_from(line)
            .ok()
            .and_then(|line| self.lines.get(line.saturating_sub(1)))
            .copied()
            .unwrap_or(self.sql.len());
        let characters = usize::try_from(column.saturating_sub(1)).unwrap_or(usize::MAX);
        self.sql[line_start..]
            .char_indices()
            .nth(characters)
            .map_or(self.sql.len(), |(index, _)| line_start + index)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec};

    use super::{BeginMode, SourceStatement, StatementError, TransactionControl, single, split};

    fn plain(text: &str) -> SourceStatement<'_> {
        SourceStatement::Plain(text)
    }

    #[test]
    fn splits_on_top_level_semicolons_only() {
        assert_eq!(
            split("SELECT ';' AS a; -- c;\nSELECT \"x;y\" FROM t; ;  ").unwrap(),
            vec![plain("SELECT ';' AS a"), plain("SELECT \"x;y\" FROM t")],
        );
    }

    #[test]
    fn keeps_trigger_body_whole() {
        let trigger = "CREATE TEMP TRIGGER t AFTER INSERT ON a BEGIN INSERT INTO b VALUES (1); UPDATE b SET c = CASE WHEN 1 THEN 2 END; END";
        assert_eq!(
            split(&alloc::format!("{trigger}; SELECT 1")).unwrap(),
            vec![plain(trigger), plain("SELECT 1")],
        );
        assert_eq!(
            split("CREATE TRIGGER t AFTER INSERT ON a BEGIN SELECT 1;"),
            Err(StatementError::UnterminatedTrigger),
        );
    }

    #[test]
    fn recognises_controls_after_comments_and_quotes() {
        assert_eq!(
            split("/* x */ BEGIN IMMEDIATE; savepoint \"Outer\"; RELEASE Outer; ROLLBACK TO SAVEPOINT Outer; ROLLBACK; END TRANSACTION; COMMIT").unwrap(),
            vec![
                SourceStatement::Control(TransactionControl::Begin(BeginMode::Immediate)),
                SourceStatement::Control(TransactionControl::Savepoint("Outer".to_string())),
                SourceStatement::Control(TransactionControl::Release("Outer".to_string())),
                SourceStatement::Control(TransactionControl::RollbackTo("Outer".to_string())),
                SourceStatement::Control(TransactionControl::Rollback { chain: false }),
                SourceStatement::Control(TransactionControl::Commit { chain: false }),
                SourceStatement::Control(TransactionControl::Commit { chain: false }),
            ],
        );
        assert_eq!(split("\"begin\"").unwrap(), vec![plain("\"begin\"")]);
        assert_eq!(split("EXPLAIN BEGIN").unwrap(), vec![plain("EXPLAIN BEGIN")]);
    }

    #[test]
    fn single_refuses_several_statements() {
        assert_eq!(single("SELECT 1; SELECT 2"), Err(StatementError::StatementCount(2)));
        assert_eq!(single(" ; "), Err(StatementError::StatementCount(0)));
        assert_eq!(single("SELECT 'é'; "), Ok(plain("SELECT 'é'")));
    }

    #[test]
    fn refuses_chained_controls() {
        assert!(matches!(split("COMMIT AND CHAIN"), Err(StatementError::UnsupportedControl(_))));
    }
}
