// storage/mem.rs
use std::collections::HashMap;

use crate::engine::table::Table;
use crate::ast::ColumnDef;
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

    fn rows<'a>(&'a self, table: &str)
        -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String>
    {
        let key = table.to_lowercase();
        let t = self.tables.get(&key).ok_or("table not found")?;
        Ok(Box::new(t.rows.iter()))
    }
}
