// src/engine.rs
pub mod value;
mod table;
mod catalog;
mod executor; // 実働部隊

pub use executor::{QueryResult}; // 外に見せたい結果型だけ再公開

use crate::ast::Stmt;
use catalog::Catalog;

/// 外から使う唯一の窓口
#[derive(Debug, Default)]
pub struct Engine {
    catalog: Catalog,
}

impl Engine {
    pub fn new() -> Self {
        Self { catalog: Catalog::new() }
    }

    /// ASTを受けて実行はexecutorへ委譲
    pub fn execute(&mut self, stmt: Stmt) -> Result<QueryResult, String> {
        executor::execute(self, stmt)
    }

    // executor から内部状態に触らせるための限定公開アクセサ
    pub(crate) fn catalog(&self) -> &Catalog { &self.catalog }
    pub(crate) fn catalog_mut(&mut self) -> &mut Catalog { &mut self.catalog }
}
