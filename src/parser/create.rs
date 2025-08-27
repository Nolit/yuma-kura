use crate::ast::{ColumnType, ColumnDef, Stmt};

pub fn parse_create_table(s: &str) -> Result<Stmt, String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() < 3 || !tokens[0].eq_ignore_ascii_case("create") || !tokens[1].eq_ignore_ascii_case("table") {
        return Err("expected: CREATE TABLE <name> (...)".into());
    }
    let name = tokens[2].trim_matches('"').to_string();
    if name.is_empty() { return Err("table name missing".into()); }

    let paren_start = s.find('(').ok_or("CREATE TABLE expects column list in (...) ")?;
    let paren_end = s.rfind(')').ok_or("missing closing ) in column list")?;
    if paren_end <= paren_start + 1 { return Err("empty column list".into()); }
    let inner = &s[paren_start + 1 .. paren_end];

    let mut columns = Vec::new();
    for raw in inner.split(',') {
        let part = raw.trim();
        if part.is_empty() { continue; }
        let mut it = part.split_whitespace();
        let col_name = it.next().ok_or("column name expected")?.trim_matches('"').to_string();
        let ty_tok = it.next().ok_or("column type expected (e.g., INT, TEXT, BOOL)")?;
        let data_type = match_ty(ty_tok)?;
        columns.push(ColumnDef { name: col_name, data_type });
    }

    if columns.is_empty() { return Err("no columns".into()); }
    Ok(Stmt::CreateTable { table: name, columns })
}

fn match_ty(tok: &str) -> Result<ColumnType, String> {
    let t = tok.to_lowercase();
    match t.as_str() {
        "int" | "integer" => Ok(ColumnType::Int),
        "text" | "string" | "varchar" => Ok(ColumnType::Text),
        "bool" | "boolean" => Ok(ColumnType::Bool),
        _ => Err(format!("unsupported type: {tok}")),
    }
}