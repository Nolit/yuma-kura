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

/// A failure inside `commit_changes` (WAL append, fsync or writing the real files).
/// After it the storage state cannot be trusted, so the process must stop and let
/// Recovery run at the next start. It is a different type from the ordinary SQL
/// error (`String`) on purpose, so callers cannot mix the two up.
/// It only carries a message; no storage-specific structure leaks through it.
#[derive(Debug, Clone, PartialEq)]
pub struct FatalError(pub String);

impl std::fmt::Display for FatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FatalError {}

pub trait Storage {
    fn create_table(&mut self, name: &str, schema: Vec<ColumnDef>) -> Result<(), String>;
    fn insert_row(&mut self, table: &str, row: Row) -> Result<(), String>;
    fn update_rows(&mut self, table: &str, assignments: &[crate::ast::Assignment], filter: Option<&crate::ast::Predicate>) -> Result<usize, String>;
    fn delete_rows(&mut self, table: &str, filter: Option<&crate::ast::Predicate>) -> Result<usize, String>;

    /// Make the changes of the SQL that just succeeded durable (auto commit).
    /// Every error is a `FatalError`: the caller must stop the process.
    fn commit_changes(&mut self) -> Result<(), FatalError>;

    /// Discard the uncommitted changes of an SQL that failed before `commit_changes`.
    /// Only in-memory state is thrown away, so there is no error to return.
    fn abort_changes(&mut self);

    /// Returns table schema as a read-only slice
    fn schema(&self, table: &str) -> Result<&[ColumnDef], String>;

    /// Returns an iterator over rows by reference (boxed for future extensibility)
    fn rows<'a>(&'a self, table: &str)
        -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String>;

    /// Get all rows as owned values (for CSV storage compatibility)
    fn get_all_rows(&self, table: &str) -> Result<Vec<Row>, String> {
        // Default implementation for memory storage
        let iter = self.rows(table)?;
        Ok(iter.cloned().collect())
    }

    /// Get streaming iterator over rows (for CSV storage)
    fn rows_streaming(&self, table: &str) -> Result<Box<dyn Iterator<Item = Result<Row, String>> + Send>, String> {
        // Default implementation falls back to get_all_rows
        let rows = self.get_all_rows(table)?;
        Ok(Box::new(rows.into_iter().map(Ok)))
    }
}

pub mod mem;
pub mod csv;
pub mod paged;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ColumnDef, ColumnType};
    use crate::engine::value::Value;

    fn int_schema() -> Vec<ColumnDef> {
        vec![ColumnDef { name: "id".into(), data_type: ColumnType::Int }]
    }

    /// Mem and Csv write the real data when the SQL runs, so commit_changes and
    /// abort_changes must neither fail nor change what is stored.
    fn assert_commit_and_abort_are_no_ops(storage: &mut dyn Storage) {
        storage.create_table("t", int_schema()).unwrap();
        storage.insert_row("t", vec![Value::Int(1)]).unwrap();

        assert_eq!(storage.commit_changes(), Ok(()));
        assert_eq!(storage.get_all_rows("t").unwrap(), vec![vec![Value::Int(1)]]);

        storage.insert_row("t", vec![Value::Int(2)]).unwrap();
        storage.abort_changes(); // cannot discard: the row stays
        assert_eq!(
            storage.get_all_rows("t").unwrap(),
            vec![vec![Value::Int(1)], vec![Value::Int(2)]]
        );
    }

    #[test]
    fn mem_storage_commit_and_abort_are_no_ops() {
        assert_commit_and_abort_are_no_ops(&mut mem::MemStorage::default());
    }

    #[test]
    fn csv_storage_commit_and_abort_are_no_ops() {
        let dir = std::env::temp_dir()
            .join(format!("mydb_csv_commit_{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_dir_all(&dir);
        let mut storage = csv::CsvStorage::new(dir.clone()).unwrap();
        assert_commit_and_abort_are_no_ops(&mut storage);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paged_storage_commit_and_abort_work_through_the_trait() {
        let dir = std::env::temp_dir()
            .join(format!("mydb_paged_trait_{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_dir_all(&dir);
        let mut storage = paged::PagedStorage::new(dir.clone()).unwrap();
        let dyn_storage: &mut dyn Storage = &mut storage;

        dyn_storage.create_table("t", int_schema()).unwrap();
        assert_eq!(dyn_storage.commit_changes(), Ok(()));
        dyn_storage.insert_row("t", vec![Value::Int(1)]).unwrap();
        assert_eq!(dyn_storage.commit_changes(), Ok(()));

        // an uncommitted INSERT is thrown away by abort_changes
        dyn_storage.insert_row("t", vec![Value::Int(2)]).unwrap();
        dyn_storage.abort_changes();
        assert_eq!(dyn_storage.get_all_rows("t").unwrap(), vec![vec![Value::Int(1)]]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
mod pending;
mod wal;
// Only the dump entry point leaves the Storage layer (used by `Engine::dump_wal`);
// WAL record types stay private to this layer.
pub use wal::dump as dump_wal;