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
            StorageType::Paged(path) => {
                match PagedStorage::new(path) {
                    Ok(paged_storage) => Box::new(paged_storage),
                    Err(e) => {
                        eprintln!("Warning: Failed to initialize Paged storage: {}, falling back to memory", e);
                        Box::new(MemStorage::default())
                    }
                }
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
