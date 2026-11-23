// src/engine/executor.rs
use crate::ast::{Stmt, ColumnType, Predicate, CmpOp};
use super::{Engine, ExecuteError, value::Value};

#[derive(Debug)]
pub enum QueryResult {
    Acknowledged,
    Rows { columns: Vec<String>, rows: Vec<Vec<Value>> },
}

/// Run a data-changing statement and commit it (auto commit).
/// - `run` fails: `abort_changes()` discards the uncommitted changes, and the error is a
///   normal SQL error (`ExecuteError::Sql`).
/// - `run` succeeds: `commit_changes()` makes it durable. Its error is fatal and is passed
///   on as it is (`ExecuteError::Fatal`); `abort_changes()` is NOT called, because the
///   WAL may already hold the commit and recovery must decide at the next start.
fn run_and_commit<T>(
    engine: &mut Engine,
    run: impl FnOnce(&mut Engine) -> Result<T, String>,
) -> Result<T, ExecuteError> {
    match run(engine) {
        Ok(value) => {
            engine.storage_mut().commit_changes().map_err(ExecuteError::Fatal)?;
            Ok(value)
        }
        Err(message) => {
            engine.storage_mut().abort_changes();
            Err(ExecuteError::Sql(message))
        }
    }
}

pub(super) fn execute(engine: &mut Engine, stmt: Stmt) -> Result<QueryResult, ExecuteError> {
    match stmt {
        Stmt::CreateTable { table, columns } => {
            run_and_commit(engine, |e| e.storage_mut().create_table(&table, columns))?;
            Ok(QueryResult::Acknowledged)
        }
        Stmt::InsertValues { table, values } => {
            run_and_commit(engine, |e| {
                let schema = e.storage().schema(&table)?;

                if values.len() != schema.len() {
                    return Err(format!(
                        "column count mismatch: expected {}, got {}",
                        schema.len(), values.len()
                    ));
                }

                let parsed = values.iter().zip(schema)
                    .map(|(raw, col)| parse_value(raw, &col.data_type)
                         .map_err(|e| format!("column {}: {e}", col.name)))
                    .collect::<Result<Vec<_>, _>>()?;

                e.storage_mut().insert_row(&table, parsed)
            })?;
            Ok(QueryResult::Acknowledged)
        }
        Stmt::Select { table, columns: _, filter } => {
            let schema = engine.storage().schema(&table)?;
            let cols = schema.iter().map(|c| c.name.clone()).collect();
            
            // Use streaming iterator for better memory efficiency
            let mut all_rows = Vec::new();
            let mut streaming_iter = engine.storage().rows_streaming(&table)?;
            
            while let Some(row_result) = streaming_iter.next() {
                let row = row_result?;
                
                // Apply filter if specified
                let matches = if let Some(ref predicate) = filter {
                    evaluate_predicate(predicate, &row, &schema)
                } else {
                    true
                };
                
                if matches {
                    all_rows.push(row);
                }
            }
            
            Ok(QueryResult::Rows { columns: cols, rows: all_rows })
        }
        Stmt::Update { table, assignments, filter } => {
            let updated_count = run_and_commit(engine, |e| {
                // Reject unknown columns instead of silently matching zero rows
                {
                    let schema = e.storage().schema(&table)?;
                    if let Some(ref predicate) = filter {
                        check_predicate_columns(predicate, &schema)?;
                    }
                    for a in &assignments {
                        if !schema.iter().any(|c| c.name == a.column) {
                            return Err(format!("column '{}' not found in table '{}'", a.column, table));
                        }
                    }
                }
                e.storage_mut().update_rows(&table, &assignments, filter.as_ref())
            })?;
            // Report only after the commit succeeded
            println!("Updated {} rows", updated_count);
            Ok(QueryResult::Acknowledged)
        }
        Stmt::Delete { table, filter } => {
            let deleted_count = run_and_commit(engine, |e| {
                e.storage_mut().delete_rows(&table, filter.as_ref())
            })?;
            println!("Deleted {} rows", deleted_count);
            Ok(QueryResult::Acknowledged)
        }
    }
}


fn parse_value(raw: &str, ty: &ColumnType) -> Result<Value, String> {
    match ty {
        ColumnType::Int => raw.parse::<i64>().map(Value::Int)
            .map_err(|_| format!("expected INT, got {raw:?}")),
        ColumnType::Text => Ok(Value::Text(raw.to_string())),
        ColumnType::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("expected BOOL, got {raw:?}")),
        },
    }
}

fn check_predicate_columns(predicate: &Predicate, schema: &[crate::ast::ColumnDef]) -> Result<(), String> {
    match predicate {
        Predicate::Cmp { col, .. } => {
            if schema.iter().any(|c| &c.name == col) {
                Ok(())
            } else {
                Err(format!("column '{}' not found", col))
            }
        }
        Predicate::And(l, r) | Predicate::Or(l, r) => {
            check_predicate_columns(l, schema)?;
            check_predicate_columns(r, schema)
        }
    }
}

fn evaluate_predicate(predicate: &Predicate, row: &[Value], schema: &[crate::ast::ColumnDef]) -> bool {
    match predicate {
        Predicate::Cmp { col, op, val } => {
            // Get column index from column name
            if let Some(col_idx) = schema.iter().position(|c| &c.name == col) {
                if let Some(row_val) = row.get(col_idx) {
                    return evaluate_comparison(row_val, op, val);
                }
            }
            false
        }
        Predicate::And(left, right) => {
            evaluate_predicate(left, row, schema) && evaluate_predicate(right, row, schema)
        }
        Predicate::Or(left, right) => {
            evaluate_predicate(left, row, schema) || evaluate_predicate(right, row, schema)
        }
    }
}

fn evaluate_comparison(left: &Value, op: &CmpOp, right: &Value) -> bool {
    match (left, right) {
        (Value::Int(l), Value::Int(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            CmpOp::Lt => l < r,
            CmpOp::Le => l <= r,
            CmpOp::Gt => l > r,
            CmpOp::Ge => l >= r,
        },
        (Value::Text(l), Value::Text(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            CmpOp::Lt => l < r,
            CmpOp::Le => l <= r,
            CmpOp::Gt => l > r,
            CmpOp::Ge => l >= r,
        },
        (Value::Bool(l), Value::Bool(r)) => match op {
            CmpOp::Eq => l == r,
            CmpOp::Ne => l != r,
            _ => false, // Size comparison is invalid for boolean values
        },
        _ => false, // Cannot compare values of different types
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ColumnDef, Assignment, Projection};
    use crate::storage::{Storage, FatalError, Row};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Calls {
        commits: u32,
        aborts: u32,
    }

    /// A storage that only records commit/abort calls and can be told to fail
    struct MockStorage {
        schema: Vec<ColumnDef>,
        calls: Rc<RefCell<Calls>>,
        fail_insert: bool,
        fail_commit: bool,
    }

    impl Storage for MockStorage {
        fn create_table(&mut self, _name: &str, _schema: Vec<ColumnDef>) -> Result<(), String> { Ok(()) }
        fn insert_row(&mut self, _table: &str, _row: Row) -> Result<(), String> {
            if self.fail_insert { Err("insert failed".to_string()) } else { Ok(()) }
        }
        fn update_rows(&mut self, _t: &str, _a: &[Assignment], _f: Option<&Predicate>) -> Result<usize, String> { Ok(1) }
        fn delete_rows(&mut self, _t: &str, _f: Option<&Predicate>) -> Result<usize, String> { Ok(1) }
        fn commit_changes(&mut self) -> Result<(), FatalError> {
            self.calls.borrow_mut().commits += 1;
            if self.fail_commit { Err(FatalError("disk gone".to_string())) } else { Ok(()) }
        }
        fn abort_changes(&mut self) {
            self.calls.borrow_mut().aborts += 1;
        }
        fn schema(&self, _table: &str) -> Result<&[ColumnDef], String> { Ok(&self.schema) }
        fn rows<'a>(&'a self, _table: &str) -> Result<Box<dyn Iterator<Item = &'a Row> + 'a>, String> {
            Ok(Box::new(std::iter::empty()))
        }
    }

    fn engine_with(fail_insert: bool, fail_commit: bool) -> (Engine, Rc<RefCell<Calls>>) {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let storage = MockStorage {
            schema: vec![ColumnDef { name: "id".into(), data_type: ColumnType::Int }],
            calls: Rc::clone(&calls),
            fail_insert,
            fail_commit,
        };
        (Engine { storage: Box::new(storage) }, calls)
    }

    fn insert(values: &[&str]) -> Stmt {
        Stmt::InsertValues { table: "t".into(), values: values.iter().map(|v| v.to_string()).collect() }
    }

    #[test]
    fn successful_statement_is_committed_once_and_not_aborted() {
        let (mut engine, calls) = engine_with(false, false);
        assert!(execute(&mut engine, insert(&["1"])).is_ok());
        assert_eq!((calls.borrow().commits, calls.borrow().aborts), (1, 0));

        let create = Stmt::CreateTable { table: "t".into(), columns: vec![] };
        assert!(execute(&mut engine, create).is_ok());
        let update = Stmt::Update { table: "t".into(), assignments: vec![], filter: None };
        assert!(execute(&mut engine, update).is_ok());
        let delete = Stmt::Delete { table: "t".into(), filter: None };
        assert!(execute(&mut engine, delete).is_ok());
        assert_eq!((calls.borrow().commits, calls.borrow().aborts), (4, 0));
    }

    #[test]
    fn storage_error_before_commit_aborts_and_is_a_sql_error() {
        let (mut engine, calls) = engine_with(true, false);
        let result = execute(&mut engine, insert(&["1"]));
        assert!(matches!(result, Err(ExecuteError::Sql(ref m)) if m == "insert failed"));
        assert_eq!((calls.borrow().commits, calls.borrow().aborts), (0, 1));
    }

    #[test]
    fn validation_error_before_commit_aborts_and_is_a_sql_error() {
        let (mut engine, calls) = engine_with(false, false);
        // two values for a one-column table
        let result = execute(&mut engine, insert(&["1", "2"]));
        assert!(matches!(result, Err(ExecuteError::Sql(ref m)) if m.contains("column count mismatch")));
        assert_eq!((calls.borrow().commits, calls.borrow().aborts), (0, 1));

        // unknown column in UPDATE
        let update = Stmt::Update {
            table: "t".into(),
            assignments: vec![Assignment { column: "nope".into(), value: "1".into() }],
            filter: None,
        };
        assert!(matches!(execute(&mut engine, update), Err(ExecuteError::Sql(_))));
        assert_eq!((calls.borrow().commits, calls.borrow().aborts), (0, 2));
    }

    #[test]
    fn commit_error_is_passed_on_as_fatal_without_abort() {
        let (mut engine, calls) = engine_with(false, true);
        let result = execute(&mut engine, insert(&["1"]));
        assert!(matches!(result, Err(ExecuteError::Fatal(FatalError(ref m))) if m == "disk gone"));
        assert_eq!((calls.borrow().commits, calls.borrow().aborts), (1, 0));
    }

    #[test]
    fn select_neither_commits_nor_aborts() {
        let (mut engine, calls) = engine_with(false, false);
        let select = Stmt::Select { table: "t".into(), columns: Projection::All, filter: None };
        assert!(matches!(execute(&mut engine, select), Ok(QueryResult::Rows { .. })));
        assert_eq!((calls.borrow().commits, calls.borrow().aborts), (0, 0));
    }

    #[test]
    fn statements_through_a_paged_engine_are_written_to_the_wal_and_survive_reopening() {
        use crate::storage::paged::PagedStorage;
        let dir = std::env::temp_dir()
            .join(format!("mydb_engine_paged_{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let _ = std::fs::remove_dir_all(&dir);
        let wal_len = |dir: &str| std::fs::metadata(std::path::Path::new(dir).join("wal.log")).unwrap().len();
        let mut engine = Engine { storage: Box::new(PagedStorage::new(dir.clone()).unwrap()) };

        let columns = vec![ColumnDef { name: "id".into(), data_type: ColumnType::Int }];
        execute(&mut engine, Stmt::CreateTable { table: "t".into(), columns }).unwrap();
        execute(&mut engine, insert(&["1"])).unwrap();
        execute(&mut engine, insert(&["2"])).unwrap();
        let len_after_inserts = wal_len(&dir);
        assert!(len_after_inserts > 0);

        // failing statements change neither the data nor the WAL
        assert!(matches!(execute(&mut engine, insert(&["1", "2"])), Err(ExecuteError::Sql(_))));
        assert!(matches!(execute(&mut engine, insert(&["not a number"])), Err(ExecuteError::Sql(_))));
        assert_eq!(wal_len(&dir), len_after_inserts);

        // the engine keeps working after the failures
        let delete = Stmt::Delete {
            table: "t".into(),
            filter: Some(Predicate::Cmp { col: "id".into(), op: CmpOp::Eq, val: Value::Int(1) }),
        };
        execute(&mut engine, delete).unwrap();
        assert!(wal_len(&dir) > len_after_inserts);
        drop(engine);

        let reopened = PagedStorage::new(dir.clone()).unwrap();
        assert_eq!(reopened.get_all_rows("t").unwrap(), vec![vec![Value::Int(2)]]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
