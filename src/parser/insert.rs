use crate::ast::Stmt;


pub fn parse_insert_values(s: &str) -> Result<Stmt, String> {
    let lower = s.to_lowercase();
    let into_pos = lower.find("into ").ok_or("expected: INSERT INTO ...")?;
    let values_pos = lower.find(" values").ok_or("expected: ... VALUES (...)")?;
    if !lower.starts_with("insert ") || values_pos <= into_pos { return Err("bad INSERT syntax".into()); }

    let table = s[into_pos + 5 .. values_pos].trim().trim_matches('"').to_string();
    if table.is_empty() { return Err("table name missing".into()); }

    let paren_start = s[values_pos..].find('(').ok_or("VALUES expects (...)")? + values_pos;
    let paren_end   = s[values_pos..].rfind(')').ok_or("VALUES missing closing )")? + values_pos;
    if paren_end <= paren_start { return Err("empty VALUES list".into()); }
    let inner = &s[paren_start+1 .. paren_end];

    let values = inner
        .split(',')
        .map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>();

    if values.is_empty() { return Err("no values".into()); }
    Ok(Stmt::InsertValues { table, values })
}