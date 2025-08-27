#[derive(Debug)]
pub enum ColumnType {
    Int,
    Text,
    Bool,
}

#[derive(Debug)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: ColumnType,
}

#[derive(Debug)]
pub enum Stmt {
    SelectAll {table: String},
    InsertValues {table: String, values: Vec<String>},
    CreateTable {table: String, columns: Vec<ColumnDef>},
}

// Strictly speaking, this is not a SQL AST, so it might be separated later
#[derive(Debug)]
pub enum Command {
    Meta(MetaCmd),
    Sql(Stmt),
}

#[derive(Debug)]
pub enum MetaCmd {Exit, Help}