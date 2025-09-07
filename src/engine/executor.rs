// src/engine/executor.rs
use crate::ast::{Stmt, ColumnType};
use crate::storage::Storage;
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
            let rows: Vec<Vec<Value>> = engine.storage().rows(&table)?.cloned().collect();
            Ok(QueryResult::Rows { columns: cols, rows })
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
