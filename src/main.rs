use std::io::{self, Write};

mod ast;
pub mod parser;
use parser::parse_sql;
use ast::{Command, MetaCmd};
mod engine;
use engine::{Engine, QueryResult};
mod storage;

fn main() {
    let user_name = "yuma";
    let mut engine = Engine::new();
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

