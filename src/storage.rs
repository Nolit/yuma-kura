use crate::ast::ColumnDef;
use crate::engine::value::Value;
pub type Row = Vec<Value>;
pub trait Storage {
    fn create_table(&mut self, name: &str, schema: Vec<ColumnDef>) -> Result<(), String>;
    fn insert_row(&mut self, table: &str, row: Row) -> Result<(), String>;

    /// Returns table schema as a read-only slice
    fn schema(&self, table: &str) -> Result<&[ColumnDef], String>;

    /// Returns an iterator over rows by reference (boxed for future extensibility)
    fn rows<'a>(&'a self, table: &str)
        -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String>;
}

pub mod mem;