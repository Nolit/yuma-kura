// storage/mem.rs
use std::collections::HashMap;

use crate::engine::table::Table;
use crate::ast::{ColumnDef, Assignment, Predicate, CmpOp};
use crate::engine::value::Value;
use super::{Storage, Row};

#[derive(Default)]
pub struct MemStorage {
    tables: HashMap<String, Table>,
}

impl Storage for MemStorage {
    fn create_table(&mut self, name: &str, schema: Vec<ColumnDef>) -> Result<(), String> {
        let key = name.to_lowercase();
        if self.tables.contains_key(&key) {
            return Err("table exists".into());
        }
        let tbl = Table::new(name.to_string(), schema);
        self.tables.insert(key, tbl);
        Ok(())
    }

    fn insert_row(&mut self, table: &str, row: Row) -> Result<(), String> {
        let key = table.to_lowercase();
        let t = self.tables.get_mut(&key).ok_or("table not found")?;

        if row.len() != t.schema.len() {
            return Err("column count mismatch".into());
        }

        t.rows.push(row);
        Ok(())
    }

    fn schema(&self, table: &str) -> Result<&[ColumnDef], String> {
        let key = table.to_lowercase();
        self.tables
            .get(&key)
            .map(|t| t.schema.as_slice())
            .ok_or("table not found".into())
    }

    fn update_rows(&mut self, table: &str, assignments: &[Assignment], filter: Option<&Predicate>) -> Result<usize, String> {
        let key = table.to_lowercase();
        let t = self.tables.get_mut(&key).ok_or("table not found")?;
        
        let mut updated_count = 0;
        
        for row in &mut t.rows {
            // Check if row matches the filter condition
            let matches = if let Some(predicate) = filter {
                evaluate_predicate(predicate, row, &t.schema)
            } else {
                true // No filter means update all rows
            };
            
            if matches {
                // Apply assignments
                for assignment in assignments {
                    if let Some(col_idx) = t.schema.iter().position(|c| c.name == assignment.column) {
                        let parsed_value = parse_value(&assignment.value, &t.schema[col_idx].data_type)?;
                        row[col_idx] = parsed_value;
                    } else {
                        return Err(format!("Column '{}' not found", assignment.column));
                    }
                }
                updated_count += 1;
            }
        }
        
        Ok(updated_count)
    }

    fn delete_rows(&mut self, table: &str, filter: Option<&Predicate>) -> Result<usize, String> {
        let key = table.to_lowercase();
        let t = self.tables.get_mut(&key).ok_or("table not found")?;
        
        let mut deleted_count = 0;
        
        // Filter rows to keep (those that don't match the condition)
        t.rows.retain(|row| {
            let matches = if let Some(predicate) = filter {
                evaluate_predicate(predicate, row, &t.schema)
            } else {
                true // No filter means delete all rows
            };
            
            if matches {
                deleted_count += 1;
                false // Remove this row
            } else {
                true // Keep this row
            }
        });
        
        Ok(deleted_count)
    }

    fn rows<'a>(&'a self, table: &str)
        -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String>
    {
        let key = table.to_lowercase();
        let t = self.tables.get(&key).ok_or("table not found")?;
        Ok(Box::new(t.rows.iter()))
    }
}

fn parse_value(raw: &str, ty: &crate::ast::ColumnType) -> Result<Value, String> {
    match ty {
        crate::ast::ColumnType::Int => raw.parse::<i64>().map(Value::Int)
            .map_err(|_| format!("expected INT, got {raw:?}")),
        crate::ast::ColumnType::Text => Ok(Value::Text(raw.to_string())),
        crate::ast::ColumnType::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("expected BOOL, got {raw:?}")),
        },
    }
}

fn evaluate_predicate(predicate: &Predicate, row: &[Value], schema: &[ColumnDef]) -> bool {
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
