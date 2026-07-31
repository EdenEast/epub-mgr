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
    enrichment::{
        merge::{enrich_metadata, EnrichmentReport, EnrichmentStatus, FieldPatch},
        providers::ChainedMetadataProvider,
        EnrichmentConfig, EnrichmentMode, MetadataProvider,
    },
    epub_metadata::{read_embedded_metadata, NormalizedMetadata},
    epub_writer::{copy_epub_with_metadata_patches, EpubWriteError},
    output_path::{render_output_path, NormalizedMetadata as OutputPathMetadata},
};

pub const DEFAULT_OUTPUT_PATH_TEMPLATE: &str = "{author}/[{series}/{series_index:02} ]{title}.epub";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeConfig {
    pub source_library: PathBuf,
    pub output_library: PathBuf,
    pub output_path_template: String,
    pub dry_run: bool,
    pub enrichment: Option<EnrichmentConfig>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportConfig {
    pub source_library: PathBuf,
    pub output_library: PathBuf,
    pub template: String,
    pub dry_run: bool,
    pub enrich: bool,
    pub apply_enrichment: bool,
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
    pub enrichment: Option<EnrichmentReport>,
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
    let provider = ChainedMetadataProvider::default();
    normalize_with_optional_provider(config, Some(&provider))
}

fn normalize_with_optional_provider(
    config: NormalizeConfig,
    provider: Option<&dyn MetadataProvider>,
) -> Result<NormalizeReport, NormalizeError> {
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
        .map(|source_path| build_entry(source_path, &config, provider, &mut planned_output_paths))
        .collect();

    let totals = totals_for(&entries);
    let enrich = config.enrichment.is_some();
    let apply_enrichment = config
        .enrichment
        .as_ref()
        .is_some_and(|enrichment| enrichment.mode == EnrichmentMode::AutoApplyHighConfidence);

    Ok(NormalizeReport {
        config: ReportConfig {
            source_library: config.source_library,
            output_library: config.output_library,
            template: config.output_path_template,
            dry_run: config.dry_run,
            enrich,
            apply_enrichment,
        },
        totals,
        entries,
    })
}

fn build_entry(
    source_path: PathBuf,
    config: &NormalizeConfig,
    provider: Option<&dyn MetadataProvider>,
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
                enrichment: None,
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

    let (effective_metadata, enrichment_report, applied_patches) =
        match (&config.enrichment, provider) {
            (Some(enrichment_config), Some(provider)) => {
                let outcome = enrich_metadata(&metadata, provider, enrichment_config.mode);
                warnings.extend(outcome.report.warnings.clone());
                let applied_patches = outcome
                    .report
                    .patches
                    .iter()
                    .filter(|patch| patch.applied)
                    .cloned()
                    .collect();
                (outcome.metadata, Some(outcome.report), applied_patches)
            }
            _ => (metadata.clone(), None, Vec::new()),
        };

    if !config.dry_run
        && enrichment_report
            .as_ref()
            .is_some_and(|report| report.status == EnrichmentStatus::LookupFailed)
    {
        return ReportEntry {
            source_path,
            output_path: None,
            action: EntryAction::Error,
            metadata: Some(metadata),
            enrichment: enrichment_report,
            warnings,
            error: Some(ReportError {
                code: "enrichment_lookup_failed".to_string(),
                message: "metadata enrichment lookup failed; no Cleaned EPUB was written"
                    .to_string(),
            }),
        };
    }

    let output_path_metadata = output_path_metadata(&effective_metadata);

    match render_output_path(&config.output_path_template, &output_path_metadata) {
        Ok(rendered) => {
            warnings.extend(rendered.warnings);
            let relative_output_path = rendered.relative_path;
            let final_output_path = config.output_library.join(&relative_output_path);

            if !planned_output_paths.insert(relative_output_path.clone()) {
                return skipped_entry(
                    source_path,
                    relative_output_path,
                    effective_metadata,
                    enrichment_report,
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
                    metadata: Some(effective_metadata),
                    enrichment: enrichment_report,
                    warnings,
                    error: None,
                };
            }

            copy_entry(
                source_path,
                final_output_path,
                relative_output_path,
                effective_metadata,
                enrichment_report,
                applied_patches,
                warnings,
            )
        }
        Err(error) => ReportEntry {
            source_path,
            output_path: None,
            action: EntryAction::Error,
            metadata: Some(effective_metadata),
            enrichment: enrichment_report,
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
    enrichment: Option<EnrichmentReport>,
    applied_patches: Vec<FieldPatch>,
    warnings: Vec<String>,
) -> ReportEntry {
    if !applied_patches.is_empty() {
        return match copy_epub_with_metadata_patches(
            &source_path,
            &final_output_path,
            &applied_patches,
        ) {
            Ok(()) => ReportEntry {
                source_path,
                output_path: Some(report_output_path),
                action: EntryAction::Copied,
                metadata: Some(metadata),
                enrichment,
                warnings,
                error: None,
            },
            Err(EpubWriteError::OutputExists) => skipped_entry(
                source_path,
                report_output_path,
                metadata,
                enrichment,
                warnings,
                "output_exists",
                "output path already exists",
            ),
            Err(error) => ReportEntry {
                source_path,
                output_path: Some(report_output_path),
                action: EntryAction::Error,
                metadata: Some(metadata),
                enrichment,
                warnings,
                error: Some(ReportError {
                    code: "metadata_write_error".to_string(),
                    message: error.to_string(),
                }),
            },
        };
    }

    match copy_cleaned_epub(&source_path, &final_output_path) {
        Ok(CopyOutcome::Copied) => ReportEntry {
            source_path,
            output_path: Some(report_output_path),
            action: EntryAction::Copied,
            metadata: Some(metadata),
            enrichment,
            warnings,
            error: None,
        },
        Ok(CopyOutcome::OutputExists) => skipped_entry(
            source_path,
            report_output_path,
            metadata,
            enrichment,
            warnings,
            "output_exists",
            "output path already exists",
        ),
        Err(error) => ReportEntry {
            source_path,
            output_path: Some(report_output_path),
            action: EntryAction::Error,
            metadata: Some(metadata),
            enrichment,
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
    enrichment: Option<EnrichmentReport>,
    warnings: Vec<String>,
    code: &str,
    message: &str,
) -> ReportEntry {
    ReportEntry {
        source_path,
        output_path: Some(output_path.clone()),
        action: EntryAction::Skipped,
        metadata: Some(metadata),
        enrichment,
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

    use crate::enrichment::{
        CandidateEvidence, Confidence, EnrichmentCandidate, LookupRequest, Provenance,
        ProvenancedValue, ProviderError,
    };

    struct FakeProvider(Vec<EnrichmentCandidate>);

    impl MetadataProvider for FakeProvider {
        fn lookup(
            &self,
            _request: &LookupRequest,
        ) -> Result<Vec<EnrichmentCandidate>, ProviderError> {
            Ok(self.0.clone())
        }
    }

    struct FailingProvider;

    impl MetadataProvider for FailingProvider {
        fn lookup(
            &self,
            _request: &LookupRequest,
        ) -> Result<Vec<EnrichmentCandidate>, ProviderError> {
            Err(ProviderError::new("Open Library", "network unavailable"))
        }
    }

    fn high_confidence_value(value: &str) -> ProvenancedValue<String> {
        ProvenancedValue {
            value: value.to_string(),
            confidence: Confidence::High,
            provenance: Provenance {
                source: "Wikidata".to_string(),
                record_id: "Q2136877".to_string(),
                url: "https://www.wikidata.org/wiki/Q2136877".to_string(),
            },
        }
    }

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
            enrichment: None,
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
    fn dry_run_enrichment_reports_proposed_series_without_writing_output_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp.path().join("output-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            complete_opf("The Way of Kings"),
        );

        let candidate = EnrichmentCandidate {
            series: Some(high_confidence_value("The Stormlight Archive")),
            series_index: Some(high_confidence_value("1")),
            evidence: CandidateEvidence {
                identifier_match: true,
                title_author_match: true,
                structured_series: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let report = normalize_with_optional_provider(
            NormalizeConfig {
                source_library,
                output_library: output_library.clone(),
                output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
                dry_run: true,
                enrichment: Some(EnrichmentConfig {
                    mode: EnrichmentMode::ProposeOnly,
                }),
            },
            Some(&FakeProvider(vec![candidate])),
        )
        .expect("dry-run report");

        assert!(!output_library.exists());
        assert_eq!(report.config.enrich, true);
        assert_eq!(report.entries[0].action, EntryAction::WouldCopy);
        let enrichment = report.entries[0]
            .enrichment
            .as_ref()
            .expect("enrichment report");
        assert!(!enrichment.applied);
        assert_eq!(enrichment.patches.len(), 2);
        assert_eq!(
            report.entries[0]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.series.as_deref()),
            None
        );
    }

    #[test]
    fn real_run_auto_applies_high_confidence_series_to_cleaned_epub_path_and_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp.path().join("output-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(
            &source_library.join("book.epub"),
            complete_opf("The Way of Kings"),
        );

        let candidate = EnrichmentCandidate {
            series: Some(high_confidence_value("The Stormlight Archive")),
            series_index: Some(high_confidence_value("1")),
            evidence: CandidateEvidence {
                identifier_match: true,
                title_author_match: true,
                structured_series: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let report = normalize_with_optional_provider(
            NormalizeConfig {
                source_library,
                output_library: output_library.clone(),
                output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
                dry_run: false,
                enrichment: Some(EnrichmentConfig {
                    mode: EnrichmentMode::AutoApplyHighConfidence,
                }),
            },
            Some(&FakeProvider(vec![candidate])),
        )
        .expect("real run report");

        assert_eq!(report.entries[0].action, EntryAction::Copied);
        assert_eq!(
            report.entries[0].output_path.as_deref(),
            Some(Path::new(
                "Test Author/The Stormlight Archive/01 The Way of Kings.epub"
            ))
        );
        let cleaned_epub = output_library.join(report.entries[0].output_path.as_ref().unwrap());
        let updated = read_embedded_metadata(&cleaned_epub).expect("updated metadata readable");
        assert_eq!(updated.series.as_deref(), Some("The Stormlight Archive"));
        assert_eq!(updated.series_index.as_deref(), Some("1"));
    }

    #[test]
    fn real_run_enrichment_lookup_failure_does_not_write_output() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        let output_library = temp.path().join("output-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(&source_library.join("book.epub"), complete_opf("Book"));

        let report = normalize_with_optional_provider(
            NormalizeConfig {
                source_library,
                output_library: output_library.clone(),
                output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
                dry_run: false,
                enrichment: Some(EnrichmentConfig {
                    mode: EnrichmentMode::AutoApplyHighConfidence,
                }),
            },
            Some(&FailingProvider),
        )
        .expect("real run report");

        assert_eq!(report.entries[0].action, EntryAction::Error);
        assert_eq!(
            report.entries[0]
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("enrichment_lookup_failed")
        );
        assert!(!output_library.exists());
        assert_eq!(report.totals.copied, 0);
    }

    #[test]
    fn ambiguous_enrichment_candidates_are_not_auto_applied() {
        let metadata = NormalizedMetadata {
            title: Some("Shared Title".to_string()),
            authors: vec!["Author".to_string()],
            ..Default::default()
        };
        let candidate = EnrichmentCandidate {
            series: Some(high_confidence_value("Series One")),
            evidence: CandidateEvidence {
                identifier_match: true,
                title_author_match: true,
                structured_series: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let other = EnrichmentCandidate {
            series: Some(high_confidence_value("Series Two")),
            evidence: CandidateEvidence {
                identifier_match: true,
                title_author_match: true,
                structured_series: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let outcome = enrich_metadata(
            &metadata,
            &FakeProvider(vec![candidate, other]),
            EnrichmentMode::AutoApplyHighConfidence,
        );

        assert_eq!(outcome.metadata.series, None);
        assert!(!outcome.report.applied);
        assert!(outcome
            .report
            .warnings
            .contains(&"ambiguous_enrichment_match".to_string()));
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
        assert!(!report.config.dry_run);
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
    fn dry_run_uses_calibre_series_metadata_in_default_planned_path_when_epub3_is_absent() {
        let report = dry_run_single_epub_report(
            r#"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Fallback Title</dc:title>
                <dc:creator>Fallback Author</dc:creator>
                <dc:language>en</dc:language>
                <meta name="calibre:series" content="Calibre Cycle"/>
                <meta name="calibre:series_index" content="2"/>
              </metadata>
            </package>"#,
        );

        assert_eq!(
            report.entries[0].output_path,
            Some(PathBuf::from(
                "Fallback Author/Calibre Cycle/02 Fallback Title.epub"
            ))
        );
        assert!(report.entries[0].warnings.is_empty());
        let metadata = report.entries[0].metadata.as_ref().expect("metadata");
        assert_eq!(metadata.series.as_deref(), Some("Calibre Cycle"));
        assert_eq!(metadata.series_index.as_deref(), Some("2"));
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
            enrichment: None,
        })
        .expect("dry-run report");

        assert_eq!(report.entries[0].warnings, vec!["series_conflict"]);
        let metadata = report.entries[0].metadata.as_ref().expect("metadata");
        assert_eq!(metadata.series.as_deref(), Some("EPUB Series"));
        assert_eq!(metadata.series_index.as_deref(), Some("7"));
    }

    #[test]
    fn dry_run_warns_and_chooses_first_supported_series_for_planned_path() {
        let report = dry_run_single_epub_report(
            r##"<?xml version="1.0"?>
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata>
                <dc:title>Ambiguous Title</dc:title>
                <dc:creator>Ambiguous Author</dc:creator>
                <dc:language>en</dc:language>
                <meta property="belongs-to-collection" id="first">First Cycle</meta>
                <meta property="collection-type" refines="#first">series</meta>
                <meta property="group-position" refines="#first">3</meta>
                <meta property="belongs-to-collection" id="second">Second Cycle</meta>
                <meta property="collection-type" refines="#second">series</meta>
                <meta property="group-position" refines="#second">4</meta>
              </metadata>
            </package>"##,
        );

        assert_eq!(
            report.entries[0].output_path,
            Some(PathBuf::from(
                "Ambiguous Author/First Cycle/03 Ambiguous Title.epub"
            ))
        );
        assert_eq!(report.entries[0].warnings, vec!["ambiguous_series"]);
        let metadata = report.entries[0].metadata.as_ref().expect("metadata");
        assert_eq!(metadata.series.as_deref(), Some("First Cycle"));
        assert_eq!(metadata.series_index.as_deref(), Some("3"));
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
            enrichment: None,
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
                enrich: false,
                apply_enrichment: false,
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
                enrich: false,
                apply_enrichment: false,
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
                enrichment: None,
                warnings: Vec::new(),
                error: None,
            }],
        };

        assert_eq!(
            human_summary(&report),
            "normalize summary: scanned=1 planned=1 copied=0 skipped=0 errored=0\nwould copy: source/book.epub -> Author/Series/01 Title.epub"
        );
    }

    fn dry_run_single_epub_report(opf: impl AsRef<str>) -> NormalizeReport {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_library = temp.path().join("source-library");
        fs::create_dir_all(&source_library).expect("create Source Library");
        write_epub_with_opf(&source_library.join("book.epub"), opf);

        normalize(NormalizeConfig {
            source_library,
            output_library: temp.path().join("output-library"),
            output_path_template: DEFAULT_OUTPUT_PATH_TEMPLATE.to_string(),
            dry_run: true,
            enrichment: None,
        })
        .expect("dry-run report")
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
        zip.write_all(valid_test_opf(opf.as_ref()).as_bytes())
            .expect("write OPF");
        zip.start_file("OPS/chapter.xhtml", options)
            .expect("start chapter");
        zip.write_all(b"<html><body>Chapter</body></html>")
            .expect("write chapter");
        zip.finish().expect("finish EPUB");
    }

    fn valid_test_opf(opf: &str) -> String {
        let opf = add_opf_namespace(opf);
        add_minimal_reading_order(&opf)
    }

    fn add_opf_namespace(opf: &str) -> String {
        let Some(package_start) = opf.find("<package") else {
            return opf.to_string();
        };
        let Some(relative_tag_end) = opf[package_start..].find('>') else {
            return opf.to_string();
        };
        let tag_end = package_start + relative_tag_end;
        let package_tag = &opf[package_start..tag_end];
        let mut declarations = String::new();
        if !package_tag.contains("xmlns=\"") {
            declarations.push_str(" xmlns=\"http://www.idpf.org/2007/opf\"");
        }
        if !package_tag.contains("xmlns:opf=\"") {
            declarations.push_str(" xmlns:opf=\"http://www.idpf.org/2007/opf\"");
        }
        if declarations.is_empty() {
            return opf.to_string();
        }

        let mut with_namespace = String::with_capacity(opf.len() + declarations.len());
        with_namespace.push_str(&opf[..tag_end]);
        with_namespace.push_str(&declarations);
        with_namespace.push_str(&opf[tag_end..]);
        with_namespace
    }

    fn add_minimal_reading_order(opf: &str) -> String {
        let additions = r#"
              <manifest>
                <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
              </manifest>
              <spine>
                <itemref idref="chapter"/>
              </spine>
        "#;
        opf.replacen("</package>", &format!("{additions}</package>"), 1)
    }
}
