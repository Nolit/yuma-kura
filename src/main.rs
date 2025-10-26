use std::io::{self, Write};
use std::env;

mod ast;
pub mod parser;
use parser::parse_sql;
use ast::{Command, MetaCmd};
mod engine;
use engine::{Engine, QueryResult};
mod storage;
use storage::StorageType;

fn main() {
    let args: Vec<String> = env::args().collect();
    let storage_type = args.get(1).map(|s| s.as_str()).unwrap_or("mem");
    let storage_type_enum = StorageType::from_arg(storage_type);
    
    let user_name = "yuma";
    let mut engine = match Engine::new(storage_type_enum.clone()) {
        Ok(engine) => engine,
        Err(e) => {
            eprintln!("Failed to initialize engine: {}", e);
            return;
        }
    };
    
    println!("Yuma-Kura Database Engine");
    println!("Storage: {:?}", storage_type_enum);
    println!("Type .help for available commands");
    
    loop {
        print!("{}>", user_name);
        io::stdout().flush().unwrap();

        let mut input = String::new();

        io::stdin().read_line(&mut input).expect("Failed to read line");
        let line = input.trim();
        if line.is_empty() { continue; }

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
                    Err(e) => {
                        eprintln!("Error: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }
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

