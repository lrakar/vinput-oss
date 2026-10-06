//! The windows without the engine: `cargo run -p vinput-ui --example ui --
//! settings --mock gallery --picture gallery.png`. Takes what follows
//! `vinput ui`; see `vinput_ui::Options::parse`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match vinput_ui::Options::parse(&args).and_then(|options| vinput_ui::run(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
