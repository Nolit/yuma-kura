use crate::ast::ColumnDef;
use crate::engine::value::Value;
pub type Row = Vec<Value>;
pub trait Storage {
    fn create_table(&mut self, name: &str, schema: Vec<ColumnDef>) -> Result<(), String>;
    fn insert_row(&mut self, table: &str, row: Row) -> Result<(), String>;

    /// テーブルのスキーマを読み取り専用スライスで貸す
    fn schema(&self, table: &str) -> Result<&[ColumnDef], String>;

    /// 行を参照で返すイテレータ（将来の拡張に備え Box 化）
    fn rows<'a>(&'a self, table: &str)
        -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String>;
}

pub mod mem;