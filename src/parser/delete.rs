use crate::ast::{Stmt, Predicate, CmpOp};
use crate::engine::value::Value;

pub fn parse_delete(s: &str) -> Result<Stmt, String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    
    if tokens.len() < 3 {
        return Err("expected: DELETE FROM <table> [WHERE ...];".into());
    }
    
    if !tokens[0].eq_ignore_ascii_case("delete") {
        return Err("expected DELETE".into());
    }
    
    if !tokens[1].eq_ignore_ascii_case("from") {
        return Err("expected FROM".into());
    }
    
    let table = tokens[2].trim_matches('"').to_string();
    
    // Parse WHERE clause
    let filter = if tokens.len() > 3 && tokens[3].eq_ignore_ascii_case("where") {
        parse_where_clause(&tokens[4..])?
    } else {
        None
    };
    
    Ok(Stmt::Delete { table, filter })
}

fn parse_where_clause(tokens: &[&str]) -> Result<Option<Predicate>, String> {
    if tokens.is_empty() {
        return Err("WHERE clause is empty".into());
    }
    
    // Support simple comparison conditions only: column op value
    if tokens.len() >= 3 {
        let col = tokens[0].to_string();
        let op = parse_cmp_op(tokens[1])?;
        let val = parse_value(tokens[2])?;
        
        return Ok(Some(Predicate::Cmp { col, op, val }));
    }
    
    Err("expected: column operator value".into())
}

fn parse_cmp_op(op: &str) -> Result<CmpOp, String> {
    match op {
        "=" => Ok(CmpOp::Eq),
        "!=" | "<>" => Ok(CmpOp::Ne),
        "<" => Ok(CmpOp::Lt),
        "<=" => Ok(CmpOp::Le),
        ">" => Ok(CmpOp::Gt),
        ">=" => Ok(CmpOp::Ge),
        _ => Err(format!("unsupported operator: {}", op))
    }
}

fn parse_value(s: &str) -> Result<Value, String> {
    // String literal (enclosed in single quotes)
    if s.starts_with('\'') && s.ends_with('\'') && s.len() > 1 {
        return Ok(Value::Text(s[1..s.len()-1].to_string()));
    }
    
    // Numeric value
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Value::Int(n));
    }
    
    // Boolean value
    match s.to_lowercase().as_str() {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ => Err(format!("unable to parse value: {}", s))
    }
}
