#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Text(String),
    Bool(bool),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{n}"),
            Value::Text(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{b}"),
        }
    }
}
