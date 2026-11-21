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
        Stmt::Select { table, columns: _, filter } => {
            let schema = engine.storage().schema(&table)?;
            let cols = schema.iter().map(|c| c.name.clone()).collect();
            
            // Use streaming iterator for better memory efficiency
            let mut all_rows = Vec::new();
            let mut streaming_iter = engine.storage().rows_streaming(&table)?;
            
            while let Some(row_result) = streaming_iter.next() {
                let row = row_result?;
                
                // Apply filter if specified
                let matches = if let Some(ref predicate) = filter {
                    evaluate_predicate(predicate, &row, &schema)
                } else {
                    true
                };
                
                if matches {
                    all_rows.push(row);
                }
            }
            
            Ok(QueryResult::Rows { columns: cols, rows: all_rows })
        }
        Stmt::Update { table, assignments, filter } => {
            // Reject unknown columns instead of silently matching zero rows
            {
                let schema = engine.storage().schema(&table)?;
                if let Some(ref predicate) = filter {
                    check_predicate_columns(predicate, &schema)?;
                }
                for a in &assignments {
                    if !schema.iter().any(|c| c.name == a.column) {
                        return Err(format!("column '{}' not found in table '{}'", a.column, table));
                    }
                }
            }
            let updated_count = engine.storage_mut().update_rows(&table, &assignments, filter.as_ref())?;
            println!("Updated {} rows", updated_count);
            Ok(QueryResult::Acknowledged)
        }
        Stmt::Delete { table, filter } => {
            let deleted_count = engine.storage_mut().delete_rows(&table, filter.as_ref())?;
            println!("Deleted {} rows", deleted_count);
            Ok(QueryResult::Acknowledged)
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

fn check_predicate_columns(predicate: &Predicate, schema: &[crate::ast::ColumnDef]) -> Result<(), String> {
    match predicate {
        Predicate::Cmp { col, .. } => {
            if schema.iter().any(|c| &c.name == col) {
                Ok(())
            } else {
                Err(format!("column '{}' not found", col))
            }
        }
        Predicate::And(l, r) | Predicate::Or(l, r) => {
            check_predicate_columns(l, schema)?;
            check_predicate_columns(r, schema)
        }
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
