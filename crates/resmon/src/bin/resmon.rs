//! `resmon` — print a resource attribution table for this machine.
//!
//! foldwave M0, experiment E3 (`docs/design.md` section 8.7b).

use std::time::Duration;

const USAGE: &str = "\
resmon — attribute CPU, memory, disk I/O and GPU use to process trees

usage: resmon [seconds] [--trees N]

  seconds     length of the sampling window (default 10, minimum 0.1)
  --trees N   how many process trees to list (default 10, 0 to omit)
  -h, --help  this message
";

#[derive(Debug)]
struct Args {
    window: Duration,
    trees: usize,
}

/// Parse the command line, returning the usage text on a bad or absent value.
fn parse(argv: &[String]) -> Result<Args, String> {
    let mut window = 10.0f64;
    let mut trees = 10usize;
    let mut i = 0;
    let mut saw_window = false;

    while i < argv.len() {
        match argv[i].as_str() {
            "-h" | "--help" => return Err(USAGE.to_string()),
            "--trees" => {
                let v = argv.get(i + 1).ok_or("--trees needs a number")?;
                trees = v.parse().map_err(|_| format!("bad --trees value {v:?}"))?;
                i += 2;
            }
            other if !saw_window => {
                window = other
                    .parse()
                    .map_err(|_| format!("bad seconds value {other:?}"))?;
                if !(window.is_finite() && window >= 0.1) {
                    return Err("seconds must be at least 0.1".to_string());
                }
                saw_window = true;
                i += 1;
            }
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(Args {
        window: Duration::from_secs_f64(window),
        trees,
    })
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(a) => a,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    eprintln!("sampling for {:.1} s...", args.window.as_secs_f64());
    match resmon::measure(args.window) {
        Ok(at) => print!("{}", resmon::report::render(&at, args.trees)),
        Err(e) => {
            eprintln!("resmon: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_to_a_ten_second_window_and_ten_trees() {
        let a = parse(&[]).expect("no arguments is valid");
        assert_eq!(a.window, Duration::from_secs(10));
        assert_eq!(a.trees, 10);
    }

    #[test]
    fn a_bare_number_sets_the_window() {
        assert_eq!(parse(&argv(&["2.5"])).unwrap().window.as_secs_f64(), 2.5);
    }

    #[test]
    fn trees_can_be_set_and_zeroed() {
        assert_eq!(parse(&argv(&["--trees", "3"])).unwrap().trees, 3);
        assert_eq!(parse(&argv(&["--trees", "0"])).unwrap().trees, 0);
    }

    #[test]
    fn window_and_trees_combine() {
        let a = parse(&argv(&["1", "--trees", "2"])).unwrap();
        assert_eq!(a.window, Duration::from_secs(1));
        assert_eq!(a.trees, 2);
    }

    #[test]
    fn help_returns_the_usage_text() {
        assert!(parse(&argv(&["--help"])).unwrap_err().contains("usage:"));
        assert!(parse(&argv(&["-h"])).unwrap_err().contains("usage:"));
    }

    #[test]
    fn a_too_short_window_is_rejected() {
        assert!(parse(&argv(&["0"])).is_err());
        assert!(parse(&argv(&["0.01"])).is_err());
    }

    #[test]
    fn a_non_finite_window_is_rejected() {
        assert!(parse(&argv(&["inf"])).is_err());
        assert!(parse(&argv(&["NaN"])).is_err());
    }

    #[test]
    fn nonsense_arguments_are_rejected_rather_than_ignored() {
        assert!(parse(&argv(&["banana"])).is_err());
        assert!(parse(&argv(&["--trees"])).is_err());
        assert!(parse(&argv(&["--trees", "x"])).is_err());
        assert!(parse(&argv(&["1", "2"])).is_err());
        assert!(parse(&argv(&["--wat"])).is_err());
    }
}
