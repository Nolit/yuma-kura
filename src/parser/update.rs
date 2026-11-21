use crate::ast::{Stmt, Assignment, Predicate, CmpOp};
use crate::engine::value::Value;
use crate::parser::{PResult, ParseError};

pub fn parse_update(input: &str) -> PResult<Stmt> {
    let tokens: Vec<&str> = input.split_whitespace().collect();
    let mut i = 0;

    // Parse "UPDATE table_name"
    if i >= tokens.len() || tokens[i].to_lowercase() != "update" {
        return Err(ParseError::Message("Expected UPDATE".to_string()));
    }
    i += 1;

    if i >= tokens.len() {
        return Err(ParseError::Message("Expected table name after UPDATE".to_string()));
    }
    let table = tokens[i].to_string();
    i += 1;

    // Parse "SET column1=value1, column2=value2, ..."
    if i >= tokens.len() || tokens[i].to_lowercase() != "set" {
        return Err(ParseError::Message("Expected SET after table name".to_string()));
    }
    i += 1;

    let mut assignments = Vec::new();
    
    // Parse assignments until WHERE or end of statement
    while i < tokens.len() && tokens[i].to_lowercase() != "where" {
        if i >= tokens.len() {
            return Err(ParseError::Message("Expected assignment".to_string()));
        }
        
        let column = tokens[i].to_string();
        i += 1;

        if i >= tokens.len() || tokens[i] != "=" {
            return Err(ParseError::Message("Expected = after column name".to_string()));
        }
        i += 1;

        if i >= tokens.len() {
            return Err(ParseError::Message("Expected value after =".to_string()));
        }
        
        // Strip surrounding quotes, consistent with INSERT value parsing
        let value = tokens[i].trim_matches('\'').trim_matches('"').to_string();
        i += 1;

        assignments.push(Assignment { column, value });

        // Check for comma separator
        if i < tokens.len() && tokens[i] == "," {
            i += 1;
        }
    }

    // Parse WHERE clause if present
    let filter = if i < tokens.len() && tokens[i].to_lowercase() == "where" {
        i += 1;
        Some(parse_where_clause(&tokens, &mut i)?)
    } else {
        None
    };

    Ok(Stmt::Update { table, assignments, filter })
}

fn parse_where_clause(tokens: &[&str], i: &mut usize) -> PResult<Predicate> {
    if *i >= tokens.len() {
        return Err(ParseError::Message("Expected condition after WHERE".to_string()));
    }

    let column = tokens[*i].to_string();
    *i += 1;

    if *i >= tokens.len() {
        return Err(ParseError::Message("Expected operator after column name".to_string()));
    }

    let op = match tokens[*i] {
        "=" => CmpOp::Eq,
        "!=" | "<>" => CmpOp::Ne,
        "<" => CmpOp::Lt,
        "<=" => CmpOp::Le,
        ">" => CmpOp::Gt,
        ">=" => CmpOp::Ge,
        _ => return Err(ParseError::Message(format!("Unknown operator: {}", tokens[*i]))),
    };
    *i += 1;

    if *i >= tokens.len() {
        return Err(ParseError::Message("Expected value after operator".to_string()));
    }

    let value_str = tokens[*i].to_string();
    *i += 1;

    // Parse value based on its format
    let value = if value_str.starts_with('\'') && value_str.ends_with('\'') {
        // String literal
        Value::Text(value_str[1..value_str.len()-1].to_string())
    } else if value_str.parse::<i64>().is_ok() {
        // Integer
        Value::Int(value_str.parse().unwrap())
    } else if value_str.to_lowercase() == "true" || value_str == "1" {
        // Boolean true
        Value::Bool(true)
    } else if value_str.to_lowercase() == "false" || value_str == "0" {
        // Boolean false
        Value::Bool(false)
    } else {
        // Treat as string if not quoted
        Value::Text(value_str)
    };

    Ok(Predicate::Cmp { col: column, op, val: value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_value(sql: &str) -> String {
        match parse_update(sql).unwrap() {
            Stmt::Update { assignments, .. } => assignments[0].value.clone(),
            _ => panic!("expected UPDATE"),
        }
    }

    #[test]
    fn strips_double_quotes_from_value() {
        assert_eq!(first_value(r#"update t set name = "def" where id = 1"#), "def");
    }

    #[test]
    fn strips_single_quotes_from_value() {
        assert_eq!(first_value("update t set name = 'def' where id = 1"), "def");
    }
}
