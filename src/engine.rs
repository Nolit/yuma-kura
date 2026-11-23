// src/engine.rs
pub mod value;
pub mod table;
mod executor; // Core execution engine

pub use executor::{QueryResult}; // Re-export only the result type for external use

use crate::ast::Stmt;
use crate::storage::{Storage, StorageType, FatalError, mem::MemStorage, csv::CsvStorage, paged::PagedStorage};

/// Error returned by `Engine::execute`
#[derive(Debug)]
pub enum ExecuteError {
    /// An ordinary SQL error (bad input, unknown table, ...). The session can continue.
    Sql(String),
    /// `commit_changes` failed. The storage state cannot be trusted: the caller must
    /// report it and stop the process; the next start runs Recovery.
    Fatal(FatalError),
}

/// Lets the executor keep using `?` on the storage's `String` errors
impl From<String> for ExecuteError {
    fn from(message: String) -> Self {
        ExecuteError::Sql(message)
    }
}

impl std::fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecuteError::Sql(message) => write!(f, "{}", message),
            ExecuteError::Fatal(error) => write!(f, "Fatal error: {}", error),
        }
    }
}

/// The only interface for external use
pub struct Engine {
    storage: Box<dyn Storage>,
}

impl Engine {
    /// Create a new engine with the specified storage type
    pub fn new(storage_type: StorageType) -> Result<Self, String> {
        let storage: Box<dyn Storage> = match storage_type {
            StorageType::Memory => Box::new(MemStorage::default()),
            StorageType::Csv(path) => {
                match CsvStorage::new(path) {
                    Ok(csv_storage) => Box::new(csv_storage),
                    Err(e) => {
                        eprintln!("Warning: Failed to initialize CSV storage: {}, falling back to memory", e);
                        Box::new(MemStorage::default())
                    }
                }
            },
            // No fallback to memory here (unlike CSV): a failed start means Recovery failed
            // or the files are broken. Falling back would let the user keep working in
            // memory without noticing that their data on disk is damaged.
            StorageType::Paged(path) => {
                let paged_storage = PagedStorage::new(path)
                    .map_err(|e| format!("Failed to initialize paged storage: {}", e))?;
                Box::new(paged_storage)
            },
        };
        
        Ok(Self { storage })
    }

    /// Create a new engine with default memory storage
    #[allow(dead_code)]
    pub fn new_memory() -> Result<Self, String> {
        Self::new(StorageType::Memory)
    }

    /// Print the records of the WAL file at `path` (the `dump-wal` subcommand).
    /// Needs no open database: it only reads the file.
    pub fn dump_wal(path: &std::path::Path, out: &mut impl std::io::Write) -> Result<(), String> {
        crate::storage::dump_wal(path, out)
    }

    /// Delegates execution to executor after receiving AST
    pub fn execute(&mut self, stmt: Stmt) -> Result<QueryResult, ExecuteError> {
        executor::execute(self, stmt)
    }

    // Limited public accessors for executor to access internal state
    pub(crate) fn storage(&self) -> &dyn Storage { self.storage.as_ref() }
    pub(crate) fn storage_mut(&mut self) -> &mut dyn Storage { self.storage.as_mut() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A fresh empty directory for one test
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mydb_engine_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    }

    fn paged(dir: &Path) -> StorageType {
        StorageType::Paged(dir.to_string_lossy().to_string())
    }

    #[test]
    fn paged_startup_fails_without_falling_back_when_the_wal_is_corrupt() {
        let dir = test_dir("corrupt_wal");
        std::fs::write(dir.join("wal.log"), [1u8, 2, 3, 4, 5]).unwrap(); // not even a header

        let error = Engine::new(paged(&dir)).err().expect("startup must fail, not fall back to memory");

        assert!(error.contains("Failed to initialize paged storage"), "unexpected error: {}", error);
        assert!(error.contains("corrupted"), "the cause is missing: {}", error);
        // nothing was changed or created
        assert_eq!(file_names(&dir), vec!["wal.log".to_string()]);
        assert_eq!(std::fs::read(dir.join("wal.log")).unwrap(), vec![1u8, 2, 3, 4, 5]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paged_startup_fails_without_falling_back_when_catalog_json_is_lost() {
        let dir = test_dir("lost_catalog");
        // catalog.json is broken and the WAL has no CatalogWrite to recover it from
        std::fs::write(dir.join("wal.log"), []).unwrap();
        std::fs::write(dir.join("catalog.json"), b"{\"tables\": {").unwrap();

        let error = Engine::new(paged(&dir)).err().expect("startup must fail, not fall back to memory");

        assert!(error.contains("Failed to initialize paged storage"), "unexpected error: {}", error);
        assert!(error.contains("catalog.json"), "the cause is missing: {}", error);
        assert_eq!(std::fs::read(dir.join("catalog.json")).unwrap(), b"{\"tables\": {");
        assert_eq!(file_names(&dir), vec!["catalog.json".to_string(), "wal.log".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paged_startup_succeeds_on_a_healthy_directory() {
        let dir = test_dir("healthy");

        assert!(Engine::new(paged(&dir)).is_ok());

        assert!(dir.join("wal.log").exists()); // proves it is the paged storage, not memory
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn csv_startup_still_falls_back_to_memory() {
        let dir = test_dir("csv_fallback");
        // the parent of the CSV directory is a file, so the directory cannot be created
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let csv_path = blocker.join("data").to_string_lossy().to_string();

        assert!(Engine::new(StorageType::Csv(csv_path)).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
