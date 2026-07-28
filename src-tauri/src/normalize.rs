use std::{fmt, fs::File, path::PathBuf};

use serde::Serialize;

use crate::output_path::{render_output_path, NormalizedMetadata};

pub const DEFAULT_OUTPUT_PATH_TEMPLATE: &str = "{author}/[{series}/{series_index:02} ]{title}.epub";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeConfig {
    pub source_library: PathBuf,
    pub output_library: PathBuf,
    pub output_path_template: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportConfig {
    pub source_library: PathBuf,
    pub output_library: PathBuf,
    pub template: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Totals {
    pub scanned: usize,
    pub planned: usize,
    pub copied: usize,
    pub skipped: usize,
    pub errored: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportEntry {
    pub source_path: PathBuf,
    pub output_path: Option<PathBuf>,
    pub action: EntryAction,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryAction {
    WouldCopy,
    Copied,
    Skipped,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NormalizeReport {
    pub config: ReportConfig,
    pub totals: Totals,
    pub entries: Vec<ReportEntry>,
}

#[derive(Debug)]
pub enum NormalizeError {
    SourceLibraryNotDirectory(PathBuf),
    RealRunUnsupported,
    Scan { path: PathBuf, message: String },
    ReportWrite { path: PathBuf, message: String },
}

impl fmt::Display for NormalizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceLibraryNotDirectory(path) => {
                write!(
                    formatter,
                    "Source Library is not a directory: {}",
                    path.display()
                )
            }
            Self::RealRunUnsupported => write!(
                formatter,
                "normalize without --dry-run is not supported until copy behavior is implemented"
            ),
            Self::Scan { path, message } => {
                write!(formatter, "failed to scan {}: {message}", path.display())
            }
            Self::ReportWrite { path, message } => {
                write!(
                    formatter,
                    "failed to write report {}: {message}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for NormalizeError {}

pub fn normalize(config: NormalizeConfig) -> Result<NormalizeReport, NormalizeError> {
    if !config.dry_run {
        return Err(NormalizeError::RealRunUnsupported);
    }

    if !config.source_library.is_dir() {
        return Err(NormalizeError::SourceLibraryNotDirectory(
            config.source_library,
        ));
    }

    let source_paths = scan_epub_paths(&config.source_library)?;
    let entries: Vec<ReportEntry> = source_paths
        .into_iter()
        .map(|source_path| render_report_entry(source_path, &config))
        .collect();

    let scanned = entries.len();
    let planned = entries
        .iter()
        .filter(|entry| entry.action == EntryAction::WouldCopy)
        .count();
    let errored = entries
        .iter()
        .filter(|entry| entry.action == EntryAction::Error)
        .count();

    Ok(NormalizeReport {
        config: ReportConfig {
            source_library: config.source_library,
            output_library: config.output_library,
            template: config.output_path_template,
            dry_run: true,
        },
        totals: Totals {
            scanned,
            planned,
            copied: 0,
            skipped: 0,
            errored,
        },
        entries,
    })
}

fn render_report_entry(source_path: PathBuf, config: &NormalizeConfig) -> ReportEntry {
    // Real EPUB metadata extraction lands in #10. Until then, render through the
    // same seam with empty normalized metadata so fallback and error behavior is
    // visible in dry-run reports.
    let metadata = NormalizedMetadata::default();

    match render_output_path(&config.output_path_template, &metadata) {
        Ok(rendered) => ReportEntry {
            source_path,
            output_path: Some(config.output_library.join(rendered.relative_path)),
            action: EntryAction::WouldCopy,
            warnings: rendered.warnings,
            error: None,
        },
        Err(error) => ReportEntry {
            source_path,
            output_path: None,
            action: EntryAction::Error,
            warnings: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

pub fn scan_epub_paths(source_library: &std::path::Path) -> Result<Vec<PathBuf>, NormalizeError> {
    let mut paths = Vec::new();

    for entry in jwalk::WalkDir::new(source_library).follow_links(false) {
        let entry = entry.map_err(|error| NormalizeError::Scan {
            path: source_library.to_path_buf(),
            message: error.to_string(),
        })?;

        if entry.file_type().is_file() && is_epub(&entry.path()) {
            paths.push(entry.path());
        }
    }

    paths.sort();
    Ok(paths)
}

pub fn is_epub(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"))
}

pub fn human_summary(report: &NormalizeReport) -> String {
    format!(
        "normalize summary: scanned={} planned={} copied={} skipped={} errored={}",
        report.totals.scanned,
        report.totals.planned,
        report.totals.copied,
        report.totals.skipped,
        report.totals.errored
    )
}

pub fn write_json_report(
    report: &NormalizeReport,
    report_path: PathBuf,
) -> Result<(), NormalizeError> {
    let file = File::create(&report_path).map_err(|error| NormalizeError::ReportWrite {
        path: report_path.clone(),
        message: error.to_string(),
    })?;

    serde_json::to_writer_pretty(file, report).map_err(|error| NormalizeError::ReportWrite {
        path: report_path,
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dry_run_scans_nested_epubs_without_creating_output_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let nested = source_library.join("nested");
        let output_library = temp.path().join("output-library");
        fs::create_dir_all(&nested).expect("create nested Source Library");
        fs::write(source_library.join("book.epub"), b"epub").expect("write epub");
        fs::write(nested.join("other.EPUB"), b"epub").expect("write nested epub");
        fs::write(nested.join("notes.txt"), b"not epub").expect("write ignored file");

        let report = normalize(NormalizeConfig {
            source_library: source_library.clone(),
            output_library: output_library.clone(),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect("dry-run report");

        assert!(
            !output_library.exists(),
            "dry-run must not create Output Library"
        );
        assert_eq!(report.totals.scanned, 2);
        assert_eq!(report.totals.planned, 2);
        assert_eq!(report.totals.copied, 0);
        assert_eq!(report.totals.skipped, 0);
        assert_eq!(report.totals.errored, 0);
        assert_eq!(report.entries.len(), 2);
        assert!(report
            .entries
            .iter()
            .all(|entry| entry.action == EntryAction::WouldCopy
                && entry.output_path
                    == Some(output_library.join("Unknown Author/Unknown Title.epub"))
                && entry.warnings
                    == vec![
                        "missing author; using fallback Unknown Author".to_string(),
                        "missing title; using fallback Unknown Title".to_string()
                    ]
                && entry.error.is_none()));
        assert_eq!(
            report
                .entries
                .iter()
                .map(|entry| entry.source_path.clone())
                .collect::<Vec<_>>(),
            vec![source_library.join("book.epub"), nested.join("other.EPUB")]
        );
    }

    #[test]
    fn source_library_must_be_a_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("missing");

        let error = normalize(NormalizeConfig {
            source_library: missing.clone(),
            output_library: temp.path().join("output"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect_err("missing Source Library should fail");

        assert!(matches!(
            error,
            NormalizeError::SourceLibraryNotDirectory(path) if path == missing
        ));
    }

    #[test]
    fn real_run_returns_unsupported_error() {
        let temp = tempfile::tempdir().expect("tempdir");

        let error = normalize(NormalizeConfig {
            source_library: temp.path().to_path_buf(),
            output_library: temp.path().join("output"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: false,
        })
        .expect_err("real run should be unsupported for this tracer bullet");

        assert!(matches!(error, NormalizeError::RealRunUnsupported));
    }

    #[test]
    fn writes_json_report_with_config_totals_and_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let report_path = temp.path().join("report.json");
        fs::create_dir_all(&source_library).expect("create Source Library");
        fs::write(source_library.join("book.epub"), b"epub").expect("write epub");

        let report = normalize(NormalizeConfig {
            source_library: source_library.clone(),
            output_library: temp.path().join("output-library"),
            output_path_template: "{author}/{title}.epub".to_string(),
            dry_run: true,
        })
        .expect("dry-run report");
        write_json_report(&report, report_path.clone()).expect("write JSON report");

        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(report_path).expect("read JSON report"))
                .expect("parse JSON report");
        assert_eq!(
            json["config"]["source_library"],
            source_library.to_string_lossy().as_ref()
        );
        assert_eq!(json["config"]["dry_run"], true);
        assert_eq!(json["config"]["template"], "{author}/{title}.epub");
        assert_eq!(json["totals"]["scanned"], 1);
        assert_eq!(json["totals"]["planned"], 1);
        assert_eq!(
            json["entries"][0]["source_path"],
            source_library.join("book.epub").to_string_lossy().as_ref()
        );
        assert_eq!(
            json["entries"][0]["output_path"],
            temp.path()
                .join("output-library")
                .join("Unknown Author/Unknown Title.epub")
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(json["entries"][0]["action"], "would_copy");
        assert_eq!(
            json["entries"][0]["warnings"],
            serde_json::json!([
                "missing author; using fallback Unknown Author",
                "missing title; using fallback Unknown Title"
            ])
        );
        assert_eq!(json["entries"][0]["error"], serde_json::Value::Null);
    }

    #[test]
    fn dry_run_reports_path_render_errors_per_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        fs::write(source_library.join("book.epub"), b"epub").expect("write epub");

        let report = normalize(NormalizeConfig {
            source_library: source_library.clone(),
            output_library: temp.path().join("output-library"),
            output_path_template: "{series}/{title}.epub".to_string(),
            dry_run: true,
        })
        .expect("dry-run report");

        assert_eq!(report.totals.scanned, 1);
        assert_eq!(report.totals.planned, 0);
        assert_eq!(report.totals.errored, 1);
        assert_eq!(
            report.entries[0].source_path,
            source_library.join("book.epub")
        );
        assert_eq!(report.entries[0].output_path, None);
        assert_eq!(report.entries[0].action, EntryAction::Error);
        assert_eq!(
            report.entries[0].error.as_deref(),
            Some("missing required metadata field series for Output Path Template")
        );
    }

    #[test]
    fn human_summary_includes_required_counts() {
        let report = NormalizeReport {
            config: ReportConfig {
                source_library: PathBuf::from("source"),
                output_library: PathBuf::from("output"),
                template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
                dry_run: true,
            },
            totals: Totals {
                scanned: 3,
                planned: 2,
                copied: 0,
                skipped: 1,
                errored: 0,
            },
            entries: Vec::new(),
        };

        assert_eq!(
            human_summary(&report),
            "normalize summary: scanned=3 planned=2 copied=0 skipped=1 errored=0"
        );
    }
}
