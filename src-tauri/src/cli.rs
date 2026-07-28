use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand};

use crate::normalize::{
    human_summary, normalize, write_json_report, NormalizeConfig, DEFAULT_OUTPUT_PATH_TEMPLATE,
};

#[derive(Debug, Parser)]
#[command(name = "epub-mgr", about = "EPUB Manager command line tools")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Scan a Source Library and plan Cleaned EPUB output actions.
    Normalize(NormalizeArgs),
}

#[derive(Debug, Args, PartialEq, Eq)]
struct NormalizeArgs {
    /// Source Library root scanned read-only for EPUB files.
    #[arg(long)]
    source_library: PathBuf,

    /// Output Library root for planned Cleaned EPUB copies.
    #[arg(long)]
    output_library: PathBuf,

    /// Output Path Template used for planned Cleaned EPUB paths.
    #[arg(long, default_value = DEFAULT_OUTPUT_PATH_TEMPLATE)]
    template: String,

    /// Plan the run without creating Output Library directories or copying EPUBs.
    #[arg(long)]
    dry_run: bool,

    /// Optional path for a JSON report.
    #[arg(long)]
    report: Option<PathBuf>,
}

pub fn is_cli_invocation() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "normalize")
}

pub fn run_from_env() -> ExitCode {
    run_from_args(std::env::args_os())
}

fn run_from_args<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(error.exit_code() as u8);
        }
    };

    match run(cli) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<String, Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Normalize(args) => {
            let report = normalize(NormalizeConfig {
                source_library: args.source_library,
                output_library: args.output_library,
                output_path_template: args.template,
                dry_run: args.dry_run,
            })?;

            if let Some(report_path) = args.report {
                write_json_report(&report, report_path)?;
            }

            Ok(human_summary(&report))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normalize_required_args_with_dry_run_and_report() {
        let cli = Cli::try_parse_from([
            "epub-mgr",
            "normalize",
            "--source-library",
            "source",
            "--output-library",
            "output",
            "--dry-run",
            "--report",
            "report.json",
        ])
        .expect("valid normalize args");

        let Commands::Normalize(args) = cli.command;
        assert_eq!(
            args,
            NormalizeArgs {
                source_library: PathBuf::from("source"),
                output_library: PathBuf::from("output"),
                template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
                dry_run: true,
                report: Some(PathBuf::from("report.json")),
            }
        );
    }

    #[test]
    fn normalize_requires_source_library() {
        let error = Cli::try_parse_from([
            "epub-mgr",
            "normalize",
            "--output-library",
            "output",
            "--dry-run",
        ])
        .expect_err("missing Source Library should fail parsing");

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn normalize_requires_output_library() {
        let error = Cli::try_parse_from([
            "epub-mgr",
            "normalize",
            "--source-library",
            "source",
            "--dry-run",
        ])
        .expect_err("missing Output Library should fail parsing");

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn accepts_real_run_args() {
        let cli = Cli::try_parse_from([
            "epub-mgr",
            "normalize",
            "--source-library",
            "source",
            "--output-library",
            "output",
        ])
        .expect("real-run args parse");

        let Commands::Normalize(args) = cli.command;
        assert!(!args.dry_run);
        assert_eq!(args.template, DEFAULT_OUTPUT_PATH_TEMPLATE);
    }

    #[test]
    fn accepts_custom_template() {
        let cli = Cli::try_parse_from([
            "epub-mgr",
            "normalize",
            "--source-library",
            "source",
            "--output-library",
            "output",
            "--template",
            "{author}/{title}.epub",
            "--dry-run",
        ])
        .expect("custom template args parse");

        let Commands::Normalize(args) = cli.command;
        assert_eq!(args.template, "{author}/{title}.epub");
    }
}
