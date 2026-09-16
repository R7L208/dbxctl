use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|argument| argument == "version") {
        return version_response();
    }

    for argument in args {
        println!("<{}>", argument.to_string_lossy());
    }

    env::var("FAKE_DATABRICKS_EXIT_CODE")
        .ok()
        .and_then(|code| code.parse::<u8>().ok())
        .map_or(ExitCode::SUCCESS, ExitCode::from)
}

fn version_response() -> ExitCode {
    match env::var("FAKE_DATABRICKS_VERSION_MODE").as_deref() {
        Ok("old") => println!("Databricks CLI v0.199.0"),
        Ok("malformed") => println!("unexpected output"),
        Ok("non-utf8") => {
            io::stdout().write_all(&[0xff]).expect("write invalid UTF-8");
        }
        Ok("fail") => return ExitCode::from(9),
        _ => println!("Databricks CLI v0.296.0"),
    }
    ExitCode::SUCCESS
}
