mod http;
pub mod open_library;
pub mod wikidata;

use super::EnrichmentCandidate;
use super::{LookupRequest, MetadataProvider, ProviderError};

pub struct ChainedMetadataProvider {
    open_library: open_library::OpenLibraryProvider,
    wikidata: wikidata::WikidataProvider,
}

impl Default for ChainedMetadataProvider {
    fn default() -> Self {
        Self {
            open_library: open_library::OpenLibraryProvider::default(),
            wikidata: wikidata::WikidataProvider::default(),
        }
    }
}

impl MetadataProvider for ChainedMetadataProvider {
    fn lookup(&self, request: &LookupRequest) -> Result<Vec<EnrichmentCandidate>, ProviderError> {
        let mut candidates = self.open_library.lookup(request)?;

        for candidate in &mut candidates {
            if let Some(wikidata_id) = candidate.evidence_wikidata_id() {
                if let Ok(Some(series)) = self.wikidata.series_for_entity(&wikidata_id) {
                    candidate.series = Some(series.series);
                    candidate.series_index = series.series_index;
                    candidate.evidence.structured_series = true;
                    candidate.evidence.subject_only_series = false;
                }
            }
        }

        Ok(candidates)
    }
}

trait CandidateWikidataExt {
    fn evidence_wikidata_id(&self) -> Option<String>;
}

impl CandidateWikidataExt for EnrichmentCandidate {
    fn evidence_wikidata_id(&self) -> Option<String> {
        let values = [
            self.title
                .as_ref()
                .map(|value| value.provenance.record_id.as_str()),
            self.series
                .as_ref()
                .map(|value| value.provenance.record_id.as_str()),
        ];

        values.into_iter().flatten().find_map(|record_id| {
            record_id
                .split('|')
                .find(|part| part.starts_with('Q'))
                .map(str::to_string)
        })
    }
}
