mod app;
mod canvas;
mod graph;
mod hint;
mod source;
#[cfg(test)]
mod test_support;
mod trace;

use app::TrackerApp;
use source::Source;
use std::process::ExitCode;

const USAGE: &str = "usage: reacs-tracker <file> | reacs-tracker --port <port> [--close-with-game]";

fn parse_args(args: &[String]) -> Result<(Source, bool), String> {
    let mut port = None;
    let mut path = None;
    let mut close_with_game = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--close-with-game" => close_with_game = true,
            "--port" => {
                let value = args.next().ok_or("--port needs a number")?;
                port = Some(value.parse::<u16>().map_err(|_| "--port needs a number")?);
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option `{flag}`")),
            file => path = Some(file.to_string()),
        }
    }

    match (port, path) {
        (Some(port), None) => Ok((Source::Port(port), close_with_game)),
        (None, Some(path)) => Ok((Source::File(path.into()), close_with_game)),
        _ => Err("give either a file or --port".to_string()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (source, close_with_game) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let opened = match source.open_file() {
        Ok(opened) => opened,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let result = eframe::run_native(
        "reacs tracker",
        eframe::NativeOptions::default(),
        Box::new(move |context| {
            Ok(Box::new(TrackerApp::new(
                source,
                opened,
                close_with_game,
                context.egui_ctx.clone(),
            )))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_args;
    use crate::source::Source;

    fn parse(args: &[&str]) -> Result<(Source, bool), String> {
        let args: Vec<String> = args.iter().map(|x| x.to_string()).collect();
        parse_args(&args)
    }

    #[test]
    fn parses_a_file_and_a_port() {
        assert!(matches!(parse(&["run.bin"]), Ok((Source::File(_), false))));
        assert!(matches!(
            parse(&["--port", "7878", "--close-with-game"]),
            Ok((Source::Port(7878), true))
        ));
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--port"]).is_err());
        assert!(parse(&["--port", "nope"]).is_err());
        assert!(parse(&["--port", "1", "run.bin"]).is_err());
        assert!(parse(&["--wrong"]).is_err());
    }
}
