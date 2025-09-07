// src/engine/executor.rs
use crate::ast::{Stmt, ColumnType, Predicate, CmpOp};
use super::{Engine, value::Value};

#[derive(Debug)]
pub enum QueryResult {
    Acknowledged,
    Rows { columns: Vec<String>, rows: Vec<Vec<Value>> },
}

pub(super) fn execute(engine: &mut Engine, stmt: Stmt) -> Result<QueryResult, String> {
    match stmt {
        Stmt::CreateTable { table, columns } => {
            engine.storage_mut().create_table(&table, columns)?;
            Ok(QueryResult::Acknowledged)
        }
        Stmt::InsertValues { table, values } => {
            let schema = engine.storage().schema(&table)?;

            if values.len() != schema.len() {
                return Err(format!(
                    "column count mismatch: expected {}, got {}",
                    schema.len(), values.len()
                ));
            }

            let parsed = values.iter().zip(schema)
                .map(|(raw, col)| parse_value(raw, &col.data_type)
                     .map_err(|e| format!("column {}: {e}", col.name)))
                .collect::<Result<Vec<_>, _>>()?;

            engine.storage_mut().insert_row(&table, parsed)?;
            Ok(QueryResult::Acknowledged)
        }
        Stmt::Select { table, columns, filter } => {
            let schema = engine.storage().schema(&table)?;
            let cols = schema.iter().map(|c| c.name.clone()).collect();
            let all_rows: Vec<Vec<Value>> = engine.storage().rows(&table)?.cloned().collect();
            
            // Filter rows based on WHERE condition
            let filtered_rows = if let Some(predicate) = filter {
                all_rows.into_iter()
                    .filter(|row| evaluate_predicate(&predicate, row, &schema))
                    .collect()
            } else {
                all_rows
            };
            
            Ok(QueryResult::Rows { columns: cols, rows: filtered_rows })
        }
    }
}

fn parse_value(raw: &str, ty: &ColumnType) -> Result<Value, String> {
    match ty {
        ColumnType::Int => raw.parse::<i64>().map(Value::Int)
            .map_err(|_| format!("expected INT, got {raw:?}")),
        ColumnType::Text => Ok(Value::Text(raw.to_string())),
        ColumnType::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("expected BOOL, got {raw:?}")),
        },
    }
}

fn evaluate_predicate(predicate: &Predicate, row: &[Value], schema: &[crate::ast::ColumnDef]) -> bool {
    match predicate {
        Predicate::Cmp { col, op, val } => {
            // Get column index from column name
            if let Some(col_idx) = schema.iter().position(|c| &c.name == col) {
                if let Some(row_val) = row.get(col_idx) {
                    return evaluate_comparison(row_val, op, val);
                }
            }
            false
        }
        Predicate::And(left, right) => {
            evaluate_predicate(left, row, schema) && evaluate_predicate(right, row, schema)
        }
        Predicate::Or(left, right) => {
            evaluate_predicate(left, row, schema) || evaluate_predicate(right, row, schema)
        }
    }
}

fn evaluate_comparison(left: &Value, op: &CmpOp, right: &Value) -> bool {
    match (left, right) {
        (Value::Int(l), Value::Int(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            CmpOp::Lt => l < r,
            CmpOp::Le => l <= r,
            CmpOp::Gt => l > r,
            CmpOp::Ge => l >= r,
        },
        (Value::Text(l), Value::Text(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            CmpOp::Lt => l < r,
            CmpOp::Le => l <= r,
            CmpOp::Gt => l > r,
            CmpOp::Ge => l >= r,
        },
        (Value::Bool(l), Value::Bool(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            _ => false, // Size comparison is invalid for boolean values
        },
        _ => false, // Cannot compare values of different types
    }
}
