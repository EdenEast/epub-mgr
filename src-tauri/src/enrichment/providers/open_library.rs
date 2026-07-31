use serde::Deserialize;

use super::http::HttpJsonClient;
use crate::enrichment::{
    isbn_from_identifiers, CandidateEvidence, Confidence, EnrichmentCandidate, LookupRequest,
    MetadataProvider, Provenance, ProvenancedValue, ProviderError,
};

#[derive(Default)]
pub struct OpenLibraryProvider {
    client: HttpJsonClient,
}

impl MetadataProvider for OpenLibraryProvider {
    fn lookup(&self, request: &LookupRequest) -> Result<Vec<EnrichmentCandidate>, ProviderError> {
        if let Some(isbn) = isbn_from_identifiers(&request.identifiers) {
            return self.lookup_isbn(&isbn);
        }

        let Some(title) = &request.title else {
            return Ok(Vec::new());
        };
        let Some(author) = request.authors.first() else {
            return Ok(Vec::new());
        };
        self.lookup_title_author(title, author)
    }
}

impl OpenLibraryProvider {
    fn lookup_isbn(&self, isbn: &str) -> Result<Vec<EnrichmentCandidate>, ProviderError> {
        let edition_url = format!("https://openlibrary.org/isbn/{isbn}.json");
        let edition: EditionResponse = self.client.get_json(&edition_url, "Open Library")?;
        let work_key = edition.works.first().map(|work| work.key.clone());

        let search_url = format!(
            "https://openlibrary.org/search.json?isbn={isbn}&fields=key,title,author_name,series_name,series_position,subject&limit=3"
        );
        let search: SearchResponse = self.client.get_json(&search_url, "Open Library")?;
        let mut candidates = candidates_from_search(search, true);

        if candidates.is_empty() {
            candidates.push(candidate_from_edition(&edition));
        }

        if let Some(work_key) = work_key {
            let work_url = format!("https://openlibrary.org{work_key}.json");
            if let Ok(work) = self
                .client
                .get_json::<WorkResponse>(&work_url, "Open Library")
            {
                for candidate in &mut candidates {
                    add_work_data(candidate, &work, &work_key, &edition.key);
                }
            }
        }

        Ok(candidates)
    }

    fn lookup_title_author(
        &self,
        title: &str,
        author: &str,
    ) -> Result<Vec<EnrichmentCandidate>, ProviderError> {
        let url = format!(
            "https://openlibrary.org/search.json?title={}&author={}&fields=key,title,author_name,series_name,series_position,subject&limit=5",
            urlencoding::encode(title),
            urlencoding::encode(author)
        );
        let search: SearchResponse = self.client.get_json(&url, "Open Library")?;
        let mut candidates = candidates_from_search(search, false);
        for candidate in &mut candidates {
            candidate.evidence.title_author_match =
                candidate_matches_request(candidate, title, author);
        }
        Ok(candidates)
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    #[serde(default, alias = "num_found", alias = "numFound")]
    num_found: usize,
    #[serde(default)]
    docs: Vec<SearchDoc>,
}

#[derive(Debug, Deserialize)]
struct SearchDoc {
    key: String,
    title: Option<String>,
    #[serde(default)]
    author_name: Vec<String>,
    #[serde(default)]
    series_name: Vec<String>,
    #[serde(default)]
    series_position: Vec<String>,
    #[serde(default)]
    subject: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EditionResponse {
    key: String,
    title: Option<String>,
    #[serde(default)]
    works: Vec<WorkRef>,
}

#[derive(Debug, Deserialize)]
struct WorkRef {
    key: String,
}

#[derive(Debug, Deserialize)]
struct WorkResponse {
    #[serde(default)]
    identifiers: WorkIdentifiers,
    #[serde(default)]
    subjects: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WorkIdentifiers {
    #[serde(default)]
    wikidata: Vec<String>,
}

pub fn candidates_from_search(
    search: SearchResponse,
    identifier_match: bool,
) -> Vec<EnrichmentCandidate> {
    let ambiguous = search.num_found > 1;
    search
        .docs
        .into_iter()
        .map(|doc| candidate_from_doc(doc, identifier_match, ambiguous))
        .collect()
}

fn candidate_from_doc(
    doc: SearchDoc,
    identifier_match: bool,
    ambiguous: bool,
) -> EnrichmentCandidate {
    let provenance = Provenance {
        source: "Open Library".to_string(),
        record_id: doc.key.clone(),
        url: format!("https://openlibrary.org{}", doc.key),
    };
    let mut candidate = EnrichmentCandidate {
        title: doc.title.map(|title| ProvenancedValue {
            value: title,
            confidence: if identifier_match {
                Confidence::High
            } else {
                Confidence::Medium
            },
            provenance: provenance.clone(),
        }),
        authors: doc
            .author_name
            .into_iter()
            .map(|author| ProvenancedValue {
                value: author,
                confidence: if identifier_match {
                    Confidence::High
                } else {
                    Confidence::Medium
                },
                provenance: provenance.clone(),
            })
            .collect(),
        series: doc.series_name.first().map(|series| ProvenancedValue {
            value: series.clone(),
            confidence: Confidence::High,
            provenance: provenance.clone(),
        }),
        series_index: doc
            .series_position
            .first()
            .map(|position| ProvenancedValue {
                value: position.clone(),
                confidence: Confidence::High,
                provenance: provenance.clone(),
            }),
        evidence: CandidateEvidence {
            identifier_match,
            title_author_match: true,
            structured_series: !doc.series_name.is_empty(),
            ambiguous,
            ..Default::default()
        },
    };

    if candidate.series.is_none() {
        if let Some(series) = doc
            .subject
            .iter()
            .find_map(|subject| subject.strip_prefix("series:"))
        {
            candidate.series = Some(ProvenancedValue {
                value: series.trim().to_string(),
                confidence: Confidence::Low,
                provenance,
            });
            candidate.evidence.subject_only_series = true;
        }
    }

    candidate
}

fn candidate_from_edition(edition: &EditionResponse) -> EnrichmentCandidate {
    let provenance = Provenance {
        source: "Open Library".to_string(),
        record_id: edition.key.clone(),
        url: format!("https://openlibrary.org{}", edition.key),
    };

    EnrichmentCandidate {
        title: edition.title.clone().map(|title| ProvenancedValue {
            value: title,
            confidence: Confidence::High,
            provenance,
        }),
        evidence: CandidateEvidence {
            identifier_match: true,
            title_author_match: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn candidate_matches_request(candidate: &EnrichmentCandidate, title: &str, author: &str) -> bool {
    let title_matches = candidate
        .title
        .as_ref()
        .is_some_and(|value| normalize_match_text(&value.value) == normalize_match_text(title));
    let author_matches = candidate
        .authors
        .first()
        .is_some_and(|value| normalize_match_text(&value.value) == normalize_match_text(author));

    title_matches && author_matches
}

fn normalize_match_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn add_work_data(
    candidate: &mut EnrichmentCandidate,
    work: &WorkResponse,
    work_key: &str,
    edition_key: &str,
) {
    if let Some(wikidata) = work.identifiers.wikidata.first() {
        if let Some(title) = &mut candidate.title {
            title.provenance.record_id = format!("{edition_key}|{work_key}|{wikidata}");
        }
    }

    if candidate.series.is_none() {
        if let Some(series) = work
            .subjects
            .iter()
            .find_map(|subject| subject.strip_prefix("series:"))
        {
            candidate.series = Some(ProvenancedValue {
                value: series.trim().to_string(),
                confidence: Confidence::Low,
                provenance: Provenance {
                    source: "Open Library".to_string(),
                    record_id: work_key.to_string(),
                    url: format!("https://openlibrary.org{work_key}"),
                },
            });
            candidate.evidence.subject_only_series = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{candidates_from_search, SearchResponse};

    #[test]
    fn title_author_search_requires_exact_normalized_match_for_auto_apply_evidence() {
        let response: SearchResponse = serde_json::from_str(
            r#"{
                "numFound": 1,
                "docs": [{
                    "key": "/works/OL1W",
                    "title": "Different Book",
                    "author_name": ["Other Author"],
                    "series_name": ["Some Series"],
                    "series_position": ["1"]
                }]
            }"#,
        )
        .expect("fixture parses");
        let mut candidates = candidates_from_search(response, false);
        for candidate in &mut candidates {
            candidate.evidence.title_author_match = super::candidate_matches_request(
                candidate,
                "The Way of Kings",
                "Brandon Sanderson",
            );
        }

        assert!(!candidates[0].evidence.title_author_match);
    }

    #[test]
    fn parses_isbn_search_fixture_for_way_of_kings() {
        let response: SearchResponse = serde_json::from_str(
            r#"{
                "numFound": 1,
                "docs": [{
                    "key": "/works/OL15358691W",
                    "title": "The Way of Kings",
                    "author_name": ["Brandon Sanderson"],
                    "subject": ["Fantasy", "series:Stormlight Archive"]
                }]
            }"#,
        )
        .expect("fixture parses");

        let candidates = candidates_from_search(response, true);

        assert_eq!(
            candidates[0].title.as_ref().unwrap().value,
            "The Way of Kings"
        );
        assert_eq!(candidates[0].authors[0].value, "Brandon Sanderson");
        assert_eq!(
            candidates[0].series.as_ref().unwrap().value,
            "Stormlight Archive"
        );
        assert!(candidates[0].evidence.subject_only_series);
    }
}
