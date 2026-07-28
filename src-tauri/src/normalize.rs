use std::{
    collections::HashSet,
    fmt,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
};

use serde::Serialize;

use crate::{
    copier::{copy_cleaned_epub, CopyOutcome},
    epub_metadata::{read_embedded_metadata, NormalizedMetadata},
    output_path::{render_output_path, NormalizedMetadata as OutputPathMetadata},
};

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
    pub metadata: Option<NormalizedMetadata>,
    pub warnings: Vec<String>,
    pub error: Option<ReportError>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportError {
    pub code: String,
    pub message: String,
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
    OutputLibraryOverlapsSourceLibrary {
        source_library: PathBuf,
        output_library: PathBuf,
    },
    Scan {
        path: PathBuf,
        message: String,
    },
    ReportWrite {
        path: PathBuf,
        message: String,
    },
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
            Self::OutputLibraryOverlapsSourceLibrary {
                source_library,
                output_library,
            } => write!(
                formatter,
                "Output Library must not overlap Source Library for a real normalize run: {} overlaps {}",
                output_library.display(),
                source_library.display()
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
    if !config.source_library.is_dir() {
        return Err(NormalizeError::SourceLibraryNotDirectory(
            config.source_library,
        ));
    }

    if !config.dry_run && output_library_overlaps_source_library(&config) {
        return Err(NormalizeError::OutputLibraryOverlapsSourceLibrary {
            source_library: config.source_library,
            output_library: config.output_library,
        });
    }

    let source_paths = scan_epub_paths(&config.source_library)?;
    let mut planned_output_paths = HashSet::new();
    let entries: Vec<ReportEntry> = source_paths
        .into_iter()
        .map(|source_path| build_entry(source_path, &config, &mut planned_output_paths))
        .collect();

    let totals = totals_for(&entries);

    Ok(NormalizeReport {
        config: ReportConfig {
            source_library: config.source_library,
            output_library: config.output_library,
            template: config.output_path_template,
            dry_run: config.dry_run,
        },
        totals,
        entries,
    })
}

fn build_entry(
    source_path: PathBuf,
    config: &NormalizeConfig,
    planned_output_paths: &mut HashSet<PathBuf>,
) -> ReportEntry {
    let metadata = match read_embedded_metadata(&source_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return ReportEntry {
                source_path,
                output_path: None,
                action: EntryAction::Error,
                metadata: None,
                warnings: Vec::new(),
                error: Some(ReportError {
                    code: error.code().to_string(),
                    message: error.to_string(),
                }),
            }
        }
    };

    let mut warnings = metadata.missing_required_warnings();
    warnings.extend(metadata.warnings.clone());
    let output_path_metadata = output_path_metadata(&metadata);

    match render_output_path(&config.output_path_template, &output_path_metadata) {
        Ok(rendered) => {
            warnings.extend(rendered.warnings);
            let relative_output_path = rendered.relative_path;
            let final_output_path = config.output_library.join(&relative_output_path);

            if !planned_output_paths.insert(relative_output_path.clone()) {
                return skipped_entry(
                    source_path,
                    relative_output_path,
                    metadata,
                    warnings,
                    "run_collision",
                    "multiple source EPUBs render to the same output path",
                );
            }

            if config.dry_run {
                return ReportEntry {
                    source_path,
                    output_path: Some(relative_output_path),
                    action: EntryAction::WouldCopy,
                    metadata: Some(metadata),
                    warnings,
                    error: None,
                };
            }

            copy_entry(
                source_path,
                final_output_path,
                relative_output_path,
                metadata,
                warnings,
            )
        }
        Err(error) => ReportEntry {
            source_path,
            output_path: None,
            action: EntryAction::Error,
            metadata: Some(metadata),
            warnings,
            error: Some(ReportError {
                code: "path_render_error".to_string(),
                message: error.to_string(),
            }),
        },
    }
}

fn copy_entry(
    source_path: PathBuf,
    final_output_path: PathBuf,
    report_output_path: PathBuf,
    metadata: NormalizedMetadata,
    warnings: Vec<String>,
) -> ReportEntry {
    match copy_cleaned_epub(&source_path, &final_output_path) {
        Ok(CopyOutcome::Copied) => ReportEntry {
            source_path,
            output_path: Some(report_output_path),
            action: EntryAction::Copied,
            metadata: Some(metadata),
            warnings,
            error: None,
        },
        Ok(CopyOutcome::OutputExists) => skipped_entry(
            source_path,
            report_output_path,
            metadata,
            warnings,
            "output_exists",
            "output path already exists",
        ),
        Err(error) => ReportEntry {
            source_path,
            output_path: Some(report_output_path),
            action: EntryAction::Error,
            metadata: Some(metadata),
            warnings,
            error: Some(ReportError {
                code: "copy_error".to_string(),
                message: error.to_string(),
            }),
        },
    }
}

fn skipped_entry(
    source_path: PathBuf,
    output_path: PathBuf,
    metadata: NormalizedMetadata,
    warnings: Vec<String>,
    code: &str,
    message: &str,
) -> ReportEntry {
    ReportEntry {
        source_path,
        output_path: Some(output_path.clone()),
        action: EntryAction::Skipped,
        metadata: Some(metadata),
        warnings,
        error: Some(ReportError {
            code: code.to_string(),
            message: format!("{message}: {}", output_path.display()),
        }),
    }
}

fn totals_for(entries: &[ReportEntry]) -> Totals {
    Totals {
        scanned: entries.len(),
        planned: entries
            .iter()
            .filter(|entry| entry.output_path.is_some() && entry.metadata.is_some())
            .count(),
        copied: entries
            .iter()
            .filter(|entry| entry.action == EntryAction::Copied)
            .count(),
        skipped: entries
            .iter()
            .filter(|entry| entry.action == EntryAction::Skipped)
            .count(),
        errored: entries
            .iter()
            .filter(|entry| entry.action == EntryAction::Error)
            .count(),
    }
}

fn output_path_metadata(metadata: &NormalizedMetadata) -> OutputPathMetadata {
    OutputPathMetadata {
        title: metadata.title.clone(),
        author: metadata.authors.first().cloned(),
        authors: (!metadata.authors.is_empty()).then(|| metadata.authors.join(", ")),
        author_sort: metadata.authors.first().cloned(),
        series: metadata.series.clone(),
        series_index: metadata.series_index.clone(),
        language: metadata.language.clone(),
        identifier: metadata
            .identifiers
            .iter()
            .find(|identifier| identifier.is_unique)
            .or_else(|| metadata.identifiers.first())
            .map(|identifier| identifier.value.clone()),
    }
}

fn output_library_overlaps_source_library(config: &NormalizeConfig) -> bool {
    let Ok(source_library) = config.source_library.canonicalize() else {
        return false;
    };
    let Ok(output_library) = canonicalize_existing_or_intended_path(&config.output_library) else {
        return false;
    };

    output_library.starts_with(&source_library) || source_library.starts_with(&output_library)
}

fn canonicalize_existing_or_intended_path(path: &Path) -> io::Result<PathBuf> {
    canonicalize_existing_or_intended_path_inner(&normalize_path_lexically(path)?)
}

fn canonicalize_existing_or_intended_path_inner(path: &Path) -> io::Result<PathBuf> {
    if path.try_exists()? {
        return path.canonicalize();
    }

    let Some(parent) = path.parent() else {
        return Ok(path.to_path_buf());
    };
    let Some(file_name) = path.file_name() else {
        return Ok(path.to_path_buf());
    };

    Ok(canonicalize_existing_or_intended_path_inner(parent)?.join(file_name))
}

fn normalize_path_lexically(path: &Path) -> io::Result<PathBuf> {
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();

    for component in absolute_path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    Ok(normalized)
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
    let mut summary = format!(
        "normalize summary: scanned={} planned={} copied={} skipped={} errored={}",
        report.totals.scanned,
        report.totals.planned,
        report.totals.copied,
        report.totals.skipped,
        report.totals.errored
    );

    if report.config.dry_run {
        for entry in &report.entries {
            if entry.action == EntryAction::WouldCopy {
                if let Some(output_path) = &entry.output_path {
                    summary.push_str(&format!(
                        "\nwould copy: {} -> {}",
                        entry.source_path.display(),
                        output_path.display()
                    ));
                }
            }
        }
    }

    summary
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
    use std::{fs, io::Write, path::Path};
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    #[test]
    fn dry_run_scans_nested_epubs_without_creating_output_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let nested = source_library.join("nested");
        let output_library = temp.path().join("output-library");
        fs::create_dir_all(&nested).expect("create nested Source Library");
        write_epub_with_opf(&source_library.join("book.epub"), complete_opf("Book One"));
        write_epub_with_opf(&nested.join("other.EPUB"), complete_opf("Book Two"));
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
                && entry.output_path.is_some()
                && entry.metadata.is_some()
                && entry.warnings.is_empty()
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
    fn dry_run_reports_run_collisions_without_creating_output_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp.path().join("output-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("first.epub"),
            complete_opf("Same Book"),
        );
        write_epub_with_opf(
            &source_library.join("second.epub"),
            complete_opf("Same Book"),
        );

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: output_library.clone(),
            output_path_template: "{author}/{title}.epub".to_string(),
            dry_run: true,
        })
        .expect("dry-run report");

        assert!(
            !output_library.exists(),
            "dry-run must not create Output Library"
        );
        assert_eq!(report.entries[0].action, EntryAction::WouldCopy);
        assert_eq!(report.entries[1].action, EntryAction::Skipped);
        assert_eq!(
            report.entries[1]
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("run_collision")
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
    fn real_run_copies_cleaned_epub_byte_for_byte_and_creates_parent_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp.path().join("output-library");
        let source_path = source_library.join("book.epub");
        let output_path = output_library.join("Test Author/Copied Book.epub");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(&source_path, complete_opf("Copied Book"));
        let original_bytes = fs::read(&source_path).expect("read source EPUB");

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: output_library.clone(),
            output_path_template: "{author}/{title}.epub".to_string(),
            dry_run: false,
        })
        .expect("real-run report");

        assert_eq!(
            fs::read(&output_path).expect("read Cleaned EPUB"),
            original_bytes
        );
        assert!(
            output_path.parent().expect("parent").is_dir(),
            "real run must create parent directories for Cleaned EPUBs"
        );
        assert_eq!(report.config.dry_run, false);
        assert_eq!(report.totals.scanned, 1);
        assert_eq!(report.totals.planned, 1);
        assert_eq!(report.totals.copied, 1);
        assert_eq!(report.totals.skipped, 0);
        assert_eq!(report.totals.errored, 0);
        assert_eq!(report.entries[0].action, EntryAction::Copied);
        assert_eq!(
            report.entries[0].output_path,
            Some(PathBuf::from("Test Author/Copied Book.epub"))
        );
        assert!(report.entries[0].error.is_none());
    }

    #[test]
    fn real_run_skips_existing_output_without_overwriting() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp.path().join("output-library");
        let output_path = output_library.join("Test Author/Existing Book.epub");
        fs::create_dir_all(&source_library).expect("create Source Library");
        fs::create_dir_all(output_path.parent().expect("parent")).expect("create output parent");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            complete_opf("Existing Book"),
        );
        fs::write(&output_path, b"existing output must survive").expect("write existing output");

        let report = normalize(NormalizeConfig {
            source_library,
            output_library,
            output_path_template: "{author}/{title}.epub".to_string(),
            dry_run: false,
        })
        .expect("real-run report");

        assert_eq!(
            fs::read(&output_path).expect("read existing output"),
            b"existing output must survive"
        );
        assert_eq!(report.totals.scanned, 1);
        assert_eq!(report.totals.planned, 1);
        assert_eq!(report.totals.copied, 0);
        assert_eq!(report.totals.skipped, 1);
        assert_eq!(report.totals.errored, 0);
        assert_eq!(report.entries[0].action, EntryAction::Skipped);
        let error = report.entries[0].error.as_ref().expect("skip error");
        assert_eq!(error.code, "output_exists");
    }

    #[test]
    fn real_run_report_distinguishes_copied_skipped_and_errored_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp.path().join("output-library");
        let existing_output = output_library.join("Test Author/Existing Book.epub");
        fs::create_dir_all(&source_library).expect("create Source Library");
        fs::create_dir_all(existing_output.parent().expect("parent"))
            .expect("create output parent");
        write_epub_with_opf(
            &source_library.join("copy.epub"),
            complete_opf("Copied Book"),
        );
        write_epub_with_opf(
            &source_library.join("existing.epub"),
            complete_opf("Existing Book"),
        );
        write_epub_with_opf(
            &source_library.join("malformed.epub"),
            "<package><metadata><dc:title>Broken",
        );
        fs::write(&existing_output, b"already here").expect("write existing output");

        let report = normalize(NormalizeConfig {
            source_library,
            output_library,
            output_path_template: "{author}/{title}.epub".to_string(),
            dry_run: false,
        })
        .expect("real-run report should include per-file errors");

        assert_eq!(report.totals.scanned, 3);
        assert_eq!(report.totals.planned, 2);
        assert_eq!(report.totals.copied, 1);
        assert_eq!(report.totals.skipped, 1);
        assert_eq!(report.totals.errored, 1);
        assert_eq!(report.entries[0].action, EntryAction::Copied);
        assert_eq!(report.entries[1].action, EntryAction::Skipped);
        assert_eq!(
            report.entries[1]
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("output_exists")
        );
        assert_eq!(report.entries[2].action, EntryAction::Error);
        assert_eq!(
            report.entries[2]
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("malformed_package_document")
        );
    }

    #[test]
    fn real_run_rejects_overlapping_output_library_to_protect_source_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = source_library.join("output-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(&source_library.join("book.epub"), complete_opf("Book"));

        let error = normalize(NormalizeConfig {
            source_library: source_library.clone(),
            output_library: output_library.clone(),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: false,
        })
        .expect_err("real run must not modify Source Library");

        assert!(matches!(
            error,
            NormalizeError::OutputLibraryOverlapsSourceLibrary {
                source_library: reported_source,
                output_library: reported_output,
            } if reported_source == source_library && reported_output == output_library
        ));
        assert!(
            !output_library.exists(),
            "rejected real run must not create Output Library inside Source Library"
        );
    }

    #[test]
    fn real_run_rejects_output_library_that_contains_source_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let output_library = temp.path().join("output-library");
        let source_library = output_library.join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(&source_library.join("book.epub"), complete_opf("Book"));

        let error = normalize(NormalizeConfig {
            source_library: source_library.clone(),
            output_library: output_library.clone(),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: false,
        })
        .expect_err("real run must reject overlapping libraries");

        assert!(matches!(
            error,
            NormalizeError::OutputLibraryOverlapsSourceLibrary {
                source_library: reported_source,
                output_library: reported_output,
            } if reported_source == source_library && reported_output == output_library
        ));
    }

    #[test]
    fn real_run_rejects_parent_components_that_would_place_output_library_inside_source_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp
            .path()
            .join("missing-parent")
            .join("..")
            .join("source-library")
            .join("output-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(&source_library.join("book.epub"), complete_opf("Book"));

        let error = normalize(NormalizeConfig {
            source_library: source_library.clone(),
            output_library: output_library.clone(),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: false,
        })
        .expect_err("real run must reject normalized overlap");

        assert!(matches!(
            error,
            NormalizeError::OutputLibraryOverlapsSourceLibrary { .. }
        ));
        assert!(
            !source_library.join("output-library").exists(),
            "rejected real run must not create Output Library through parent components"
        );
    }

    #[test]
    fn dry_run_extracts_embedded_package_metadata_into_report_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            r##"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/" unique-identifier="book-id">
              <metadata>
                <dc:title>Extracted Title</dc:title>
                <dc:creator opf:role="aut">Author One</dc:creator>
                <dc:creator opf:role="edt">Editor One</dc:creator>
                <dc:creator>Author Two</dc:creator>
                <dc:language>en-US</dc:language>
                <dc:identifier id="book-id" opf:scheme="uuid">urn:uuid:abc</dc:identifier>
                <dc:identifier>isbn:9780000000000</dc:identifier>
              </metadata>
            </package>"##,
        );

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: temp.path().join("output-library"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect("dry-run report");

        assert_eq!(report.totals.planned, 1);
        assert_eq!(report.totals.errored, 0);
        let metadata = report.entries[0].metadata.as_ref().expect("metadata");
        assert_eq!(metadata.title.as_deref(), Some("Extracted Title"));
        assert_eq!(metadata.authors, vec!["Author One", "Author Two"]);
        assert_eq!(metadata.language.as_deref(), Some("en-US"));
        assert_eq!(metadata.identifiers.len(), 2);
        assert!(metadata.identifiers[0].is_unique);
        assert_eq!(
            report.entries[0].output_path,
            Some(PathBuf::from("Author One/Extracted Title.epub"))
        );
    }

    #[test]
    fn dry_run_uses_embedded_series_metadata_in_default_planned_path_and_report() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp.path().join("output-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            r##"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Series Title</dc:title>
                <dc:creator>Series Author</dc:creator>
                <dc:language>en</dc:language>
                <meta property="belongs-to-collection" id="series-id">Planned Series</meta>
                <meta property="collection-type" refines="#series-id">series</meta>
                <meta property="group-position" refines="#series-id">1</meta>
              </metadata>
            </package>"##,
        );

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: output_library.clone(),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect("dry-run report");

        assert_eq!(report.totals.planned, 1);
        assert_eq!(report.totals.errored, 0);
        assert_eq!(
            report.entries[0].output_path,
            Some(PathBuf::from(
                "Series Author/Planned Series/01 Series Title.epub"
            ))
        );
        let metadata = report.entries[0].metadata.as_ref().expect("metadata");
        assert_eq!(metadata.series.as_deref(), Some("Planned Series"));
        assert_eq!(metadata.series_index.as_deref(), Some("1"));
        let json = serde_json::to_value(metadata).expect("serialize metadata");
        assert_eq!(json["series"], "Planned Series");
        assert_eq!(json["series_index"], "1");
        assert!(report.entries[0].warnings.is_empty());
    }

    #[test]
    fn dry_run_includes_series_warnings_in_report_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            r##"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Conflict Title</dc:title>
                <dc:creator>Conflict Author</dc:creator>
                <dc:language>en</dc:language>
                <meta property="belongs-to-collection" id="series-id">EPUB Series</meta>
                <meta property="collection-type" refines="#series-id">series</meta>
                <meta property="group-position" refines="#series-id">7</meta>
                <meta name="calibre:series" content="Calibre Series"/>
                <meta name="calibre:series_index" content="8"/>
              </metadata>
            </package>"##,
        );

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: temp.path().join("output-library"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect("dry-run report");

        assert_eq!(report.entries[0].warnings, vec!["series_conflict"]);
        let metadata = report.entries[0].metadata.as_ref().expect("metadata");
        assert_eq!(metadata.series.as_deref(), Some("EPUB Series"));
        assert_eq!(metadata.series_index.as_deref(), Some("7"));
    }

    #[test]
    fn dry_run_warns_when_required_metadata_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            r#"<package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:identifier>urn:uuid:missing</dc:identifier>
              </metadata>
            </package>"#,
        );

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: temp.path().join("output-library"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect("dry-run report");

        assert_eq!(report.totals.planned, 1);
        assert_eq!(report.totals.errored, 0);
        assert_eq!(report.entries[0].action, EntryAction::WouldCopy);
        assert_eq!(
            report.entries[0].warnings,
            vec![
                "missing_title",
                "missing_author",
                "missing_language",
                "missing author; using fallback Unknown Author",
                "missing title; using fallback Unknown Title"
            ]
        );
    }

    #[test]
    fn dry_run_warns_when_only_non_dublin_core_metadata_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            r#"<package xmlns:custom="urn:not-dc">
              <metadata>
                <custom:title>Wrong Title</custom:title>
                <custom:creator>Wrong Author</custom:creator>
                <custom:language>xx</custom:language>
                <custom:identifier>wrong-id</custom:identifier>
              </metadata>
            </package>"#,
        );

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: temp.path().join("output-library"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect("dry-run report");

        assert_eq!(report.totals.planned, 1);
        assert_eq!(report.totals.errored, 0);
        let metadata = report.entries[0].metadata.as_ref().expect("metadata");
        assert_eq!(metadata.title, None);
        assert_eq!(metadata.authors, Vec::<String>::new());
        assert_eq!(metadata.language, None);
        assert_eq!(metadata.identifiers, Vec::new());
        assert_eq!(
            report.entries[0].warnings,
            vec![
                "missing_title",
                "missing_author",
                "missing_language",
                "missing author; using fallback Unknown Author",
                "missing title; using fallback Unknown Title"
            ]
        );
    }

    #[test]
    fn malformed_epub_metadata_is_reported_per_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            "<package><metadata><dc:title>Broken",
        );

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: temp.path().join("output-library"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect("dry-run report should continue");

        assert_eq!(report.totals.scanned, 1);
        assert_eq!(report.totals.planned, 0);
        assert_eq!(report.totals.errored, 1);
        assert_eq!(report.entries[0].action, EntryAction::Error);
        assert_eq!(report.entries[0].metadata, None);
        let error = report.entries[0].error.as_ref().expect("entry error");
        assert_eq!(error.code, "malformed_package_document");
    }

    #[test]
    fn unreadable_epub_is_reported_per_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        fs::write(source_library.join("book.epub"), b"not a zip").expect("write invalid EPUB");

        let report = normalize(NormalizeConfig {
            source_library,
            output_library: temp.path().join("output-library"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
        })
        .expect("dry-run report should continue");

        assert_eq!(report.totals.scanned, 1);
        assert_eq!(report.totals.planned, 0);
        assert_eq!(report.totals.errored, 1);
        assert_eq!(report.entries[0].action, EntryAction::Error);
        let error = report.entries[0].error.as_ref().expect("entry error");
        assert_eq!(error.code, "unreadable_epub");
    }

    #[test]
    fn writes_json_report_with_config_totals_entries_and_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let report_path = temp.path().join("report.json");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            complete_opf("Report Title"),
        );

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
            "Test Author/Report Title.epub"
        );
        assert_eq!(json["entries"][0]["action"], "would_copy");
        assert_eq!(json["entries"][0]["metadata"]["title"], "Report Title");
        assert_eq!(
            json["entries"][0]["metadata"]["authors"],
            serde_json::json!(["Test Author"])
        );
        assert_eq!(json["entries"][0]["metadata"]["language"], "en");
        assert_eq!(json["entries"][0]["warnings"], serde_json::json!([]));
        assert_eq!(json["entries"][0]["error"], serde_json::Value::Null);
    }

    #[test]
    fn dry_run_reports_path_render_errors_per_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(&source_library.join("book.epub"), complete_opf("Book"));

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
            report.entries[0]
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("path_render_error")
        );
        assert_eq!(
            report.entries[0]
                .error
                .as_ref()
                .map(|error| error.message.as_str()),
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

    #[test]
    fn human_summary_includes_dry_run_output_paths_for_validation() {
        let report = NormalizeReport {
            config: ReportConfig {
                source_library: PathBuf::from("source"),
                output_library: PathBuf::from("output"),
                template: "{author}/[{series}/{series_index:02} ]{title}.epub".to_string(),
                dry_run: true,
            },
            totals: Totals {
                scanned: 1,
                planned: 1,
                copied: 0,
                skipped: 0,
                errored: 0,
            },
            entries: vec![ReportEntry {
                source_path: PathBuf::from("source/book.epub"),
                output_path: Some(PathBuf::from("Author/Series/01 Title.epub")),
                action: EntryAction::WouldCopy,
                metadata: None,
                warnings: Vec::new(),
                error: None,
            }],
        };

        assert_eq!(
            human_summary(&report),
            "normalize summary: scanned=1 planned=1 copied=0 skipped=0 errored=0\nwould copy: source/book.epub -> Author/Series/01 Title.epub"
        );
    }

    fn complete_opf(title: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>{title}</dc:title>
                <dc:creator>Test Author</dc:creator>
                <dc:language>en</dc:language>
              </metadata>
            </package>"#
        )
    }

    fn write_epub_with_opf(path: &Path, opf: impl AsRef<str>) {
        let file = fs::File::create(path).expect("create EPUB");
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        zip.start_file("META-INF/container.xml", options)
            .expect("start container");
        zip.write_all(
            br#"<?xml version="1.0"?>
            <container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
              <rootfiles>
                <rootfile full-path="OPS/content.opf" media-type="application/oebps-package+xml"/>
              </rootfiles>
            </container>"#,
        )
        .expect("write container");
        zip.start_file("OPS/content.opf", options)
            .expect("start OPF");
        zip.write_all(opf.as_ref().as_bytes()).expect("write OPF");
        zip.finish().expect("finish EPUB");
    }
}
