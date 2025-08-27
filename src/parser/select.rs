use crate::ast::Stmt;

pub fn parse_select_all(s: &str) -> Result<Stmt, String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() >= 4
        && tokens[0].eq_ignore_ascii_case("select")
        && tokens[1] == "*"
        && tokens[2].eq_ignore_ascii_case("from")
    {
        Ok(Stmt::SelectAll { table: tokens[3].trim_matches('"').to_string() })
    } else {
        Err("expected: SELECT * FROM <table>;".into())
    }
}