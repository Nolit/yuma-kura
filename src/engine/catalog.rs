use std::collections::HashMap;
use crate::ast::ColumnDef;
use super::table::Table;

#[derive(Default, Debug)]
pub struct Catalog {
    tables: HashMap<String, Table>,
}

impl Catalog {
    pub fn new() -> Self { Self::default() }

    pub fn create_table(&mut self, name: String, schema: Vec<ColumnDef>) -> Result<(), String> {
        if self.tables.contains_key(&name) { return Err(format!("table exists: {name}")); }
        self.tables.insert(name.clone(), Table::new(name, schema));
        println!("Created table: {:?}", self.tables);
        Ok(())
    }

    pub fn get_table(&self, name: &str) -> Option<&Table> { self.tables.get(name) }
    pub fn get_table_mut(&mut self, name: &str) -> Option<&mut Table> { self.tables.get_mut(name) }
}
