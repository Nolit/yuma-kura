// src/engine.rs
pub mod value;
pub mod table;
mod executor; // Core execution engine

pub use executor::{QueryResult}; // Re-export only the result type for external use

use crate::ast::Stmt;
use crate::storage::{Storage, mem::MemStorage};

/// The only interface for external use
pub struct Engine {
    storage: Box<dyn Storage>,
}

impl Engine {
    pub fn new() -> Self {
        Self { 
            storage: Box::new(MemStorage::default())
        }
    }

    /// Delegates execution to executor after receiving AST
    pub fn execute(&mut self, stmt: Stmt) -> Result<QueryResult, String> {
        executor::execute(self, stmt)
    }

    // Limited public accessors for executor to access internal state
    pub(crate) fn storage(&self) -> &dyn Storage { self.storage.as_ref() }
    pub(crate) fn storage_mut(&mut self) -> &mut dyn Storage { self.storage.as_mut() }
}
