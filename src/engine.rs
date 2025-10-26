// src/engine.rs
pub mod value;
pub mod table;
mod executor; // Core execution engine

pub use executor::{QueryResult}; // Re-export only the result type for external use

use crate::ast::Stmt;
use crate::storage::{Storage, StorageType, mem::MemStorage, csv::CsvStorage};

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
            StorageType::Paged(_path) => {
                // TODO: Implement paged storage
                eprintln!("Warning: Paged storage not yet implemented, falling back to memory");
                Box::new(MemStorage::default())
            },
        };
        
        Ok(Self { storage })
    }

    /// Create a new engine with default memory storage
    #[allow(dead_code)]
    pub fn new_memory() -> Result<Self, String> {
        Self::new(StorageType::Memory)
    }

    /// Delegates execution to executor after receiving AST
    pub fn execute(&mut self, stmt: Stmt) -> Result<QueryResult, String> {
        executor::execute(self, stmt)
    }

    // Limited public accessors for executor to access internal state
    pub(crate) fn storage(&self) -> &dyn Storage { self.storage.as_ref() }
    pub(crate) fn storage_mut(&mut self) -> &mut dyn Storage { self.storage.as_mut() }
}
