use crate::ast::ColumnDef;
use super::value::Value;

#[derive(Debug)]
pub struct Table {
    pub name: String,
    pub schema: Vec<ColumnDef>,
    pub rows: Vec<Vec<Value>>,
}

impl Table {
    pub fn new(name: String, schema: Vec<ColumnDef>) -> Self {
        Self { name, schema, rows: Vec::new() }
    }
}
