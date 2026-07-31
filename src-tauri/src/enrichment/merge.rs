use serde::Serialize;

use crate::epub_metadata::NormalizedMetadata;

use super::{Confidence, EnrichmentCandidate, EnrichmentMode, MetadataProvider, Provenance};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EnrichmentReport {
    pub status: EnrichmentStatus,
    pub patches: Vec<FieldPatch>,
    pub applied: bool,
    pub warnings: Vec<String>,
    pub errors: Vec<ProviderErrorReport>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentStatus {
    NotRequested,
    Found,
    NoMatch,
    NeedsConfirmation,
    LookupFailed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FieldPatch {
    pub field: MetadataField,
    pub old_value: Option<String>,
    pub new_value: String,
    pub confidence: Confidence,
    pub provenance: Provenance,
    pub applied: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetadataField {
    Title,
    Author,
    Series,
    SeriesIndex,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderErrorReport {
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichmentOutcome {
    pub metadata: NormalizedMetadata,
    pub report: EnrichmentReport,
}

pub fn enrich_metadata(
    metadata: &NormalizedMetadata,
    provider: &dyn MetadataProvider,
    mode: EnrichmentMode,
) -> EnrichmentOutcome {
    let request = super::LookupRequest::from(metadata);
    let candidates = match provider.lookup(&request) {
        Ok(candidates) => candidates,
        Err(error) => {
            return EnrichmentOutcome {
                metadata: metadata.clone(),
                report: EnrichmentReport {
                    status: EnrichmentStatus::LookupFailed,
                    patches: Vec::new(),
                    applied: false,
                    warnings: Vec::new(),
                    errors: vec![ProviderErrorReport {
                        source: error.source,
                        message: error.message,
                    }],
                },
            }
        }
    };

    let Some(candidate) = candidates.first() else {
        return EnrichmentOutcome {
            metadata: metadata.clone(),
            report: EnrichmentReport {
                status: EnrichmentStatus::NoMatch,
                patches: Vec::new(),
                applied: false,
                warnings: Vec::new(),
                errors: Vec::new(),
            },
        };
    };
    let multiple_candidates = candidates.len() > 1;
    let candidate_auto_apply = candidate_can_auto_apply(candidate, multiple_candidates);

    let mut enriched = metadata.clone();
    let mut patches = Vec::new();
    let mut warnings = Vec::new();

    if candidate.evidence.ambiguous || multiple_candidates {
        warnings.push("ambiguous_enrichment_match".to_string());
    }
    if candidate.evidence.subject_only_series {
        warnings.push("series_from_subject_hint_only".to_string());
    }

    push_patch(
        &mut patches,
        MetadataField::Title,
        metadata.title.as_deref(),
        candidate.title.as_ref(),
        candidate_auto_apply,
        mode,
    );
    push_patch(
        &mut patches,
        MetadataField::Author,
        metadata.authors.first().map(String::as_str),
        candidate.authors.first(),
        candidate_auto_apply,
        mode,
    );
    push_patch(
        &mut patches,
        MetadataField::Series,
        metadata.series.as_deref(),
        candidate.series.as_ref(),
        candidate_auto_apply,
        mode,
    );
    push_patch(
        &mut patches,
        MetadataField::SeriesIndex,
        metadata.series_index.as_deref(),
        candidate.series_index.as_ref(),
        candidate_auto_apply,
        mode,
    );

    for patch in &patches {
        if !patch.applied {
            continue;
        }

        match patch.field {
            MetadataField::Title => enriched.title = Some(patch.new_value.clone()),
            MetadataField::Author => enriched.authors = vec![patch.new_value.clone()],
            MetadataField::Series => enriched.series = Some(patch.new_value.clone()),
            MetadataField::SeriesIndex => enriched.series_index = Some(patch.new_value.clone()),
        }
    }

    let applied = patches.iter().any(|patch| patch.applied);
    let status = if candidate.evidence.ambiguous
        || multiple_candidates
        || patches.iter().any(|patch| !patch.applied)
    {
        EnrichmentStatus::NeedsConfirmation
    } else {
        EnrichmentStatus::Found
    };

    EnrichmentOutcome {
        metadata: enriched,
        report: EnrichmentReport {
            status,
            patches,
            applied,
            warnings,
            errors: Vec::new(),
        },
    }
}

fn candidate_can_auto_apply(candidate: &EnrichmentCandidate, multiple_candidates: bool) -> bool {
    !candidate.evidence.ambiguous
        && !multiple_candidates
        && candidate.evidence.title_author_match
        && (candidate.evidence.identifier_match || candidate.evidence.structured_series)
        && !candidate.evidence.subject_only_series
}

fn push_patch(
    patches: &mut Vec<FieldPatch>,
    field: MetadataField,
    old_value: Option<&str>,
    proposed: Option<&super::ProvenancedValue<String>>,
    candidate_auto_apply: bool,
    mode: EnrichmentMode,
) {
    let Some(proposed) = proposed else { return };
    if old_value.is_some_and(|old| old == proposed.value) {
        return;
    }

    let replacing_existing = old_value.is_some_and(|old| !old.trim().is_empty());
    let high_confidence = proposed.confidence == Confidence::High && candidate_auto_apply;
    let applied = mode == EnrichmentMode::AutoApplyHighConfidence
        && high_confidence
        && (!replacing_existing
            || field == MetadataField::Series
            || field == MetadataField::SeriesIndex);

    patches.push(FieldPatch {
        field,
        old_value: old_value.map(str::to_string),
        new_value: proposed.value.clone(),
        confidence: proposed.confidence.clone(),
        provenance: proposed.provenance.clone(),
        applied,
        reason: if applied {
            "high-confidence enrichment auto-applied".to_string()
        } else if replacing_existing {
            "existing metadata preserved pending confirmation".to_string()
        } else {
            "proposed for confirmation".to_string()
        },
    });
}

#[cfg(test)]
mod tests {
    use crate::{
        enrichment::{
            CandidateEvidence, Confidence, EnrichmentCandidate, EnrichmentMode, LookupRequest,
            MetadataProvider, Provenance, ProvenancedValue, ProviderError,
        },
        epub_metadata::{MetadataIdentifier, NormalizedMetadata},
    };

    use super::{enrich_metadata, MetadataField};

    struct FakeProvider(Vec<EnrichmentCandidate>);

    impl MetadataProvider for FakeProvider {
        fn lookup(
            &self,
            _request: &LookupRequest,
        ) -> Result<Vec<EnrichmentCandidate>, ProviderError> {
            Ok(self.0.clone())
        }
    }

    fn value(value: &str) -> ProvenancedValue<String> {
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
    fn auto_applies_missing_structured_series_from_identifier_match() {
        let metadata = NormalizedMetadata {
            title: Some("The Way of Kings".to_string()),
            authors: vec!["Brandon Sanderson".to_string()],
            identifiers: vec![MetadataIdentifier {
                value: "9780765365279".to_string(),
                scheme: Some("isbn".to_string()),
                is_unique: true,
            }],
            ..Default::default()
        };
        let candidate = EnrichmentCandidate {
            series: Some(value("The Stormlight Archive")),
            series_index: Some(value("1")),
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
            &FakeProvider(vec![candidate]),
            EnrichmentMode::AutoApplyHighConfidence,
        );

        assert_eq!(
            outcome.metadata.series.as_deref(),
            Some("The Stormlight Archive")
        );
        assert_eq!(outcome.metadata.series_index.as_deref(), Some("1"));
        assert!(outcome.report.applied);
    }

    #[test]
    fn subject_only_series_hint_is_not_auto_applied() {
        let metadata = NormalizedMetadata {
            title: Some("The Way of Kings".to_string()),
            authors: vec!["Brandon Sanderson".to_string()],
            ..Default::default()
        };
        let mut proposed = value("Stormlight Archive");
        proposed.confidence = Confidence::Low;
        let candidate = EnrichmentCandidate {
            series: Some(proposed),
            evidence: CandidateEvidence {
                title_author_match: true,
                subject_only_series: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let outcome = enrich_metadata(
            &metadata,
            &FakeProvider(vec![candidate]),
            EnrichmentMode::AutoApplyHighConfidence,
        );

        assert_eq!(outcome.metadata.series, None);
        assert_eq!(outcome.report.patches[0].field, MetadataField::Series);
        assert!(!outcome.report.patches[0].applied);
        assert_eq!(outcome.report.patches[0].confidence, Confidence::Low);
    }
}
