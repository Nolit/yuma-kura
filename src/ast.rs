use crate::engine::value::Value;

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
    Select {table: String, columns: Projection, filter: Option<Predicate>},
    InsertValues {table: String, values: Vec<String>},
    CreateTable {table: String, columns: Vec<ColumnDef>},
    Update {table: String, assignments: Vec<Assignment>, filter: Option<Predicate>},
}

#[derive(Debug, Clone)]
pub enum Projection { All, Cols(Vec<String>) }

#[derive(Debug, Clone)]
pub enum CmpOp { Eq, Ne, Lt, Le, Gt, Ge }

#[derive(Debug, Clone)]
pub enum Predicate {
    Cmp { col: String, op: CmpOp, val: Value },
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
}

#[derive(Debug, Clone)]
pub struct Assignment {
    pub column: String,
    pub value: String,
}

// Strictly speaking, this is not a SQL AST, so it might be separated later
#[derive(Debug)]
pub enum Command {
    Meta(MetaCmd),
    Sql(Stmt),
}

#[derive(Debug)]
pub enum MetaCmd {Exit, Help}