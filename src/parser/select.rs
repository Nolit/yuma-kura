use crate::ast::{Stmt, Projection, Predicate, CmpOp};
use crate::engine::value::Value;

pub fn parse_select_all(s: &str) -> Result<Stmt, String> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    
    if tokens.len() < 4 {
        return Err("expected: SELECT * FROM <table> [WHERE ...];".into());
    }
    
    if !tokens[0].eq_ignore_ascii_case("select") {
        return Err("expected SELECT".into());
    }
    
    if tokens[1] != "*" {
        return Err("only SELECT * is supported".into());
    }
    
    if !tokens[2].eq_ignore_ascii_case("from") {
        return Err("expected FROM".into());
    }
    
    let table = tokens[3].trim_matches('"').to_string();
    
    // WHERE句の解析
    let filter = if tokens.len() > 4 && tokens[4].eq_ignore_ascii_case("where") {
        parse_where_clause(&tokens[5..])?
    } else {
        None
    };
    
    Ok(Stmt::Select { 
        table,
        columns: Projection::All,
        filter
    })
}

fn parse_where_clause(tokens: &[&str]) -> Result<Option<Predicate>, String> {
    if tokens.is_empty() {
        return Err("WHERE clause is empty".into());
    }
    
    // 簡単な比較条件のみサポート: column op value
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
    let s = s.trim_matches('"');
    
    // 整数
    if let Ok(i) = s.parse::<i64>() {
        return Ok(Value::Int(i));
    }
    
    // ブール値
    match s.to_lowercase().as_str() {
        "true" | "1" => return Ok(Value::Bool(true)),
        "false" | "0" => return Ok(Value::Bool(false)),
        _ => {}
    }
    
    // 文字列
    Ok(Value::Text(s.to_string()))
}