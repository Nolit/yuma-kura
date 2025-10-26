mod select;
mod insert;
mod create;
mod update;
mod delete;

use crate::ast::Stmt;

use self::{select::parse_select_all, insert::parse_insert_values, create::parse_create_table, update::parse_update, delete::parse_delete};

#[derive(Debug)]
pub enum ParseError {
    Message(String),
    UnexpectedToken(String),
    Eof,
    // Add more as needed
}

pub type PResult<T> = Result<T, ParseError>;

pub fn parse_sql(input: &str) -> Result<Stmt, String> {
    let s = input.trim_end_matches(';').trim();
    let lower = s.to_lowercase();

    if lower.starts_with("select") { parse_select_all(s) }
    else if lower.starts_with("insert") { parse_insert_values(s) }
    else if lower.starts_with("create") { parse_create_table(s) }
    else if lower.starts_with("update") { parse_update(s).map_err(|e| format!("{:?}", e)) }
    else if lower.starts_with("delete") { parse_delete(s) }
    // else { Err(ParseError::Message("only SELECT/INSERT/CREATE supported".into())) }
    else { Err("only SELECT/INSERT/CREATE/UPDATE/DELETE supported".into()) }
}

