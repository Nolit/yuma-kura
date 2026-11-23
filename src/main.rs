use std::env;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

mod ast;
pub mod parser;
use parser::parse_sql;
use ast::{Command, MetaCmd};
mod engine;
use engine::{Engine, ExecuteError, QueryResult};
mod storage;
use storage::StorageType;

const HISTORY_FILE: &str = ".yumakura_history";

fn main() {
    let args: Vec<String> = env::args().collect();

    // `dump-wal <path>`: print the WAL file and exit (no database is opened)
    if let Some(path) = dump_wal_path(&args) {
        let result = path.and_then(|p| Engine::dump_wal(std::path::Path::new(p), &mut std::io::stdout()));
        if let Err(e) = result {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    let storage_type = args.get(1).map(|s| s.as_str()).unwrap_or("mem");
    let storage_type_enum = StorageType::from_arg(storage_type);
    
    let user_name = "yuma";
    let mut engine = match start_engine(storage_type_enum.clone(), &mut std::io::stderr()) {
        Ok(engine) => engine,
        Err(code) => std::process::exit(code),
    };
    
    println!("Yuma-Kura Database Engine");
    println!("Storage: {:?}", storage_type_enum);
    println!("Type .help for available commands");
    
    let mut rl = match DefaultEditor::new() {
        Ok(rl) => rl,
        Err(e) => {
            eprintln!("Failed to initialize line editor: {}", e);
            return;
        }
    };
    // 履歴ファイルが無い初回起動時はエラーになるが問題ないので無視する
    let _ = rl.load_history(HISTORY_FILE);

    loop {
        let input = match rl.readline(&format!("{}>", user_name)) {
            Ok(input) => input,
            // Ctrl+C: 入力中の行を破棄して続行
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl+D / 入力終了
            Err(ReadlineError::Eof) => {
                println!("Bye!");
                break;
            }
            Err(e) => {
                eprintln!("Failed to read line: {}", e);
                break;
            }
        };
        let line = input.trim();
        if line.is_empty() { continue; }
        let _ = rl.add_history_entry(line);

        match parse_command(line) {
            Ok(Command::Meta(MetaCmd::Exit)) => {
                println!("Bye!");
                break;
            }
            Ok(Command::Meta(MetaCmd::Help)) => {
                println!("Available commands:");
                println!("  .exit, .quit - Exit the program");
                println!("  .help - Show this help");
                println!("  SELECT * FROM <table> - Select all from table");
                println!("  INSERT INTO <table> VALUES (...) - Insert values into table");
                println!("  CREATE TABLE <table> (<columns>) - Create a new table");
                println!("  UPDATE <table> SET <column>=<value> WHERE <condition> - Update rows in table");
                println!("  DELETE FROM <table> WHERE <condition> - Delete rows from table");
                println!();
                println!("Storage options:");
                println!("  mem - In-memory storage (default)");
                println!("  csv:<path> - CSV file storage");
                println!("  paged:<file> - 4KB paged storage (future)");
            }
            Ok(Command::Sql(stmt)) => {
                println!("SQL Statement: {:?}", stmt);
                match engine.execute(stmt) {
                    Ok(QueryResult::Acknowledged) => println!("OK"),
                    Ok(QueryResult::Rows { columns, rows }) => {
                        println!("Columns: {:?}", columns);
                        println!("Rows: {:?}", rows);
                    },
                    Err(e) => match fatal_exit_code(&e) {
                        // The storage state cannot be trusted: stop here.
                        // The next start runs Recovery (history is not saved on purpose).
                        Some(code) => {
                            eprintln!("{}", e);
                            std::process::exit(code);
                        }
                        // Ordinary SQL error: report it and keep the session going
                        None => eprintln!("Error: {}", e),
                    },
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }

    if let Err(e) = rl.save_history(HISTORY_FILE) {
        eprintln!("Failed to save history: {}", e);
    }
}

/// Exit code when the engine cannot be started (e.g. Recovery failed)
const STARTUP_FAILURE_EXIT_CODE: i32 = 1;

/// Open the engine. On failure the error is written to `err` and the exit code to stop
/// the process with is returned (the caller calls `process::exit`, so this can be tested).
fn start_engine(storage_type: StorageType, err: &mut impl std::io::Write) -> Result<Engine, i32> {
    Engine::new(storage_type).map_err(|e| {
        let _ = writeln!(err, "Error: {}", e);
        STARTUP_FAILURE_EXIT_CODE
    })
}

/// The process exit code for an error from `Engine::execute`:
/// `Some(code)` if the process must stop (fatal storage error), `None` if the session can continue.
fn fatal_exit_code(error: &ExecuteError) -> Option<i32> {
    match error {
        ExecuteError::Fatal(_) => Some(1),
        ExecuteError::Sql(_) => None,
    }
}

/// `Some(..)` if the command line is the `dump-wal` subcommand:
/// the WAL file path, or a usage error when the path is missing.
fn dump_wal_path(args: &[String]) -> Option<Result<&str, String>> {
    if args.get(1).map(|s| s.as_str()) != Some("dump-wal") {
        return None;
    }
    Some(
        args.get(2)
            .map(|s| s.as_str())
            .ok_or_else(|| "Usage: dump-wal <path to wal.log>".to_string()),
    )
}

fn parse_command(line: &str) -> Result<Command, String> {
    if line.starts_with(".") { 
        parse_meta(line).map(Command::Meta)
    } else {
        parse_sql(line).map(Command::Sql)
    }
}

fn parse_meta(s: &str) -> Result<MetaCmd, String> {
    match s {
        _ if s.eq_ignore_ascii_case(".exit") || s.eq_ignore_ascii_case(".quit") => Ok(MetaCmd::Exit),
        _ if s.eq_ignore_ascii_case(".help") => Ok(MetaCmd::Help),
        _ => Err(format!("Unrecognized command: {}", s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::FatalError;

    #[test]
    fn fatal_errors_stop_the_process_with_a_non_zero_code() {
        let code = fatal_exit_code(&ExecuteError::Fatal(FatalError("disk gone".into())));
        assert_eq!(code, Some(1));
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dump_wal_subcommand_takes_the_path_argument() {
        assert_eq!(dump_wal_path(&args(&["db", "dump-wal", "x/wal.log"])), Some(Ok("x/wal.log")));
    }

    #[test]
    fn dump_wal_without_a_path_is_a_usage_error() {
        assert!(matches!(dump_wal_path(&args(&["db", "dump-wal"])), Some(Err(_))));
    }

    #[test]
    fn other_arguments_are_not_the_dump_wal_subcommand() {
        assert_eq!(dump_wal_path(&args(&["db"])), None);
        assert_eq!(dump_wal_path(&args(&["db", "paged:mydb.db"])), None);
    }

    #[test]
    fn sql_errors_keep_the_session_going() {
        assert_eq!(fatal_exit_code(&ExecuteError::Sql("table not found".into())), None);
    }

    fn startup_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mydb_main_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paged(dir: &std::path::Path) -> StorageType {
        StorageType::Paged(dir.to_string_lossy().to_string())
    }

    #[test]
    fn the_startup_failure_exit_code_is_not_zero() {
        assert_ne!(STARTUP_FAILURE_EXIT_CODE, 0);
    }

    #[test]
    fn a_corrupt_wal_fails_the_startup_with_a_non_zero_code_and_a_message() {
        let dir = startup_dir("corrupt_wal");
        std::fs::write(dir.join("wal.log"), [1u8, 2, 3, 4, 5]).unwrap();
        let mut err = Vec::new();

        let result = start_engine(paged(&dir), &mut err);

        assert_eq!(result.err(), Some(STARTUP_FAILURE_EXIT_CODE));
        let message = String::from_utf8(err).unwrap();
        assert!(message.contains("Failed to initialize paged storage"), "unexpected message: {}", message);
        assert!(message.contains("corrupted"), "the cause is missing: {}", message);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_lost_catalog_json_fails_the_startup_with_a_non_zero_code() {
        let dir = startup_dir("lost_catalog");
        std::fs::write(dir.join("wal.log"), []).unwrap();
        std::fs::write(dir.join("catalog.json"), b"{\"tables\": {").unwrap();
        let mut err = Vec::new();

        let result = start_engine(paged(&dir), &mut err);

        assert_eq!(result.err(), Some(STARTUP_FAILURE_EXIT_CODE));
        let message = String::from_utf8(err).unwrap();
        assert!(message.contains("catalog.json"), "unexpected message: {}", message);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_healthy_startup_succeeds_and_prints_nothing() {
        let dir = startup_dir("healthy");
        let mut err = Vec::new();

        assert!(start_engine(StorageType::Memory, &mut err).is_ok());
        assert!(start_engine(paged(&dir), &mut err).is_ok());

        assert!(err.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}

