// src/engine.rs
pub mod value;
pub mod table;
mod executor; // 実働部隊

pub use executor::{QueryResult}; // 外に見せたい結果型だけ再公開

use crate::ast::Stmt;
use crate::storage::{Storage, mem::MemStorage};

/// 外から使う唯一の窓口
pub struct Engine {
    storage: Box<dyn Storage>,
}

impl Engine {
    pub fn new() -> Self {
        Self { 
            storage: Box::new(MemStorage::default())
        }
    }

    /// ASTを受けて実行はexecutorへ委譲
    pub fn execute(&mut self, stmt: Stmt) -> Result<QueryResult, String> {
        executor::execute(self, stmt)
    }

    // executor から内部状態に触らせるための限定公開アクセサ
    pub(crate) fn storage(&self) -> &dyn Storage { self.storage.as_ref() }
    pub(crate) fn storage_mut(&mut self) -> &mut dyn Storage { self.storage.as_mut() }
}
