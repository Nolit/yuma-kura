// src/engine/executor.rs
use crate::ast::{Stmt, ColumnType};
use super::{Engine, value::Value};

#[derive(Debug)]
pub enum QueryResult {
    Acknowledged,
    Rows { columns: Vec<String>, rows: Vec<Vec<Value>> },
}

pub(super) fn execute(engine: &mut Engine, stmt: Stmt) -> Result<QueryResult, String> {
    match stmt {
        Stmt::CreateTable { table, columns } => {
            engine.catalog_mut().create_table(table, columns)?;
            Ok(QueryResult::Acknowledged)
        }
        Stmt::InsertValues { table, values } => {
            let tbl = engine.catalog_mut()
                .get_table_mut(&table)
                .ok_or_else(|| format!("no such table: {table}"))?;

            if values.len() != tbl.schema.len() {
                return Err(format!(
                    "column count mismatch: expected {}, got {}",
                    tbl.schema.len(), values.len()
                ));
            }

            let parsed = values.iter().zip(&tbl.schema)
                .map(|(raw, col)| parse_value(raw, &col.data_type)
                     .map_err(|e| format!("column {}: {e}", col.name)))
                .collect::<Result<Vec<_>, _>>()?;

            tbl.rows.push(parsed);
            Ok(QueryResult::Acknowledged)
        }
        Stmt::SelectAll { table } => {
            let tbl = engine.catalog()
                .get_table(&table)
                .ok_or_else(|| format!("no such table: {table}"))?;
            let cols = tbl.schema.iter().map(|c| c.name.clone()).collect();
            Ok(QueryResult::Rows { columns: cols, rows: tbl.rows.clone() })
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
