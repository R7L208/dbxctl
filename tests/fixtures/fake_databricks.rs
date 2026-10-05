use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|argument| argument == "version") {
        return version_response();
    }

    if let Some(milliseconds) = env_usize("SLEEP_MS") {
        thread::sleep(Duration::from_millis(milliseconds as u64));
    }
    if let Some(bytes) = env_usize("STDOUT_BYTES") {
        write_bytes(&mut io::stdout(), b'o', bytes, "stdout");
    }
    if let Some(bytes) = env_usize("STDERR_BYTES") {
        write_bytes(&mut io::stderr(), b'e', bytes, "stderr");
    }

    for argument in args {
        println!("<{}>", argument.to_string_lossy());
    }

    if env::var_os("FAKE_DATABRICKS_ABORT").is_some() {
        io::stdout().flush().expect("flush stdout before abort");
        // Terminates the process via SIGABRT on Unix, exercising the wrapper's
        // signal-termination exit-code path.
        std::process::abort();
    }

    env::var("FAKE_DATABRICKS_EXIT_CODE")
        .ok()
        .and_then(|code| code.parse::<u8>().ok())
        .map_or(ExitCode::SUCCESS, ExitCode::from)
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name).ok().and_then(|value| value.parse().ok())
}

fn write_bytes(stream: &mut impl Write, byte: u8, count: usize, name: &str) {
    let chunk = vec![byte; count.min(16 * 1024)];
    let mut remaining = count;
    while remaining > 0 {
        let length = remaining.min(chunk.len());
        stream
            .write_all(&chunk[..length])
            .unwrap_or_else(|error| panic!("write {name}: {error}"));
        remaining -= length;
    }
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
