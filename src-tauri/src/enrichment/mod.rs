use serde::Serialize;

use crate::epub_metadata::{MetadataIdentifier, NormalizedMetadata};

pub mod merge;
pub mod providers;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichmentConfig {
    pub mode: EnrichmentMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichmentMode {
    ProposeOnly,
    AutoApplyHighConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupRequest {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub identifiers: Vec<MetadataIdentifier>,
    pub language: Option<String>,
}

impl From<&NormalizedMetadata> for LookupRequest {
    fn from(metadata: &NormalizedMetadata) -> Self {
        Self {
            title: metadata.title.clone(),
            authors: metadata.authors.clone(),
            identifiers: metadata.identifiers.clone(),
            language: metadata.language.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Provenance {
    pub source: String,
    pub record_id: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenancedValue<T> {
    pub value: T,
    pub confidence: Confidence,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichmentCandidate {
    pub title: Option<ProvenancedValue<String>>,
    pub authors: Vec<ProvenancedValue<String>>,
    pub series: Option<ProvenancedValue<String>>,
    pub series_index: Option<ProvenancedValue<String>>,
    pub evidence: CandidateEvidence,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateEvidence {
    pub identifier_match: bool,
    pub title_author_match: bool,
    pub structured_series: bool,
    pub subject_only_series: bool,
    pub ambiguous: bool,
}

pub trait MetadataProvider {
    fn lookup(&self, request: &LookupRequest) -> Result<Vec<EnrichmentCandidate>, ProviderError>;
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderError {
    pub source: String,
    pub message: String,
}

impl ProviderError {
    pub fn new(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} lookup failed: {}", self.source, self.message)
    }
}

impl std::error::Error for ProviderError {}

pub fn isbn_from_identifiers(identifiers: &[MetadataIdentifier]) -> Option<String> {
    identifiers.iter().find_map(|identifier| {
        let scheme_is_isbn = identifier
            .scheme
            .as_deref()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("isbn"));
        let value = identifier
            .value
            .trim()
            .trim_start_matches("urn:isbn:")
            .trim_start_matches("isbn:")
            .replace(['-', ' '], "");
        let valid_len = value.len() == 10 || value.len() == 13;
        let valid_chars = value.chars().enumerate().all(|(index, ch)| {
            ch.is_ascii_digit() || (index == 9 && ch.eq_ignore_ascii_case(&'x'))
        });

        (valid_len
            && valid_chars
            && (scheme_is_isbn
                || identifier.value.to_ascii_lowercase().contains("isbn")
                || value.starts_with("978")
                || value.starts_with("979")))
        .then_some(value)
    })
}
