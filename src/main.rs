use std::io::{self, Write};

fn main() {
    let user_name = "yuma";
    loop {
        print!("{}>", user_name);
        io::stdout().flush().unwrap(); // flushしないと出ないことがある

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => {
                let trimmed = input.trim();

                // 終了コマンド
                if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
                    println!("Bye!");
                    break;
                }

                println!("You entered: {}", trimmed);
            }
            Err(error) => {
                eprintln!("Error reading input: {}", error);
            }
        }
    }
}
