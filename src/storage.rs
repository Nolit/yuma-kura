use crate::ast::ColumnDef;
use crate::engine::value::Value;
pub type Row = Vec<Value>;

/// Storage backend types supported by the engine
#[derive(Debug, Clone)]
pub enum StorageType {
    /// In-memory storage (default)
    Memory,
    /// CSV file storage (future implementation)
    Csv(String),
    /// Fixed 4KB page storage (future implementation)
    Paged(String),
}

impl Default for StorageType {
    fn default() -> Self {
        StorageType::Memory
    }
}

impl StorageType {
    /// Parse storage type from command line argument
    pub fn from_arg(arg: &str) -> Self {
        if arg.starts_with("csv:") {
            StorageType::Csv(arg[4..].to_string())
        } else if arg.starts_with("paged:") {
            StorageType::Paged(arg[6..].to_string())
        } else if arg == "mem" || arg.is_empty() {
            StorageType::Memory
        } else {
            // Default to memory for unknown types
            StorageType::Memory
        }
    }
}

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
pub mod csv;
pub mod paged;