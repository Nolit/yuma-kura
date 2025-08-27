use super::value::Value;
use crate::ast::ColumnType;

pub trait Storage {
    fn append_row(&mut self, values: Vec<Value>) -> Result<(), String>;
    fn all_rows(&self) -> Vec<Vec<Value>>;
    fn validate(&self, values: &[String], schema: &[ColumnType]) -> Result<Vec<Value>, String>;
}

// まずはテーブル自身が保持してるので、抽象だけ置いておき、
// 将来的にファイル永続化を入れるときに使う想定。
