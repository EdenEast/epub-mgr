use std::collections::HashMap;

use serde::Deserialize;

use super::http::HttpJsonClient;
use crate::enrichment::{Confidence, Provenance, ProvenancedValue, ProviderError};

const PROPERTY_PART_OF_SERIES: &str = "P179";
const PROPERTY_SERIES_ORDINAL: &str = "P1545";

#[derive(Default)]
pub struct WikidataProvider {
    client: HttpJsonClient,
}

pub struct WikidataSeries {
    pub series: ProvenancedValue<String>,
    pub series_index: Option<ProvenancedValue<String>>,
}

impl WikidataProvider {
    pub fn series_for_entity(&self, qid: &str) -> Result<Option<WikidataSeries>, ProviderError> {
        let url = format!("https://www.wikidata.org/wiki/Special:EntityData/{qid}.json");
        let entity_data: EntityData = self.client.get_json(&url, "Wikidata")?;
        let Some(entity) = entity_data.entities.get(qid) else {
            return Ok(None);
        };
        let Some(series_claim) = entity
            .claims
            .get(PROPERTY_PART_OF_SERIES)
            .and_then(|claims| claims.first())
        else {
            return Ok(None);
        };
        let Some(series_id) = claim_entity_id(series_claim) else {
            return Ok(None);
        };
        let ordinal = series_claim
            .qualifiers
            .as_ref()
            .and_then(|qualifiers| qualifiers.get(PROPERTY_SERIES_ORDINAL))
            .and_then(|values| values.first())
            .and_then(claim_string_value);

        let series_url =
            format!("https://www.wikidata.org/wiki/Special:EntityData/{series_id}.json");
        let series_data: EntityData = self.client.get_json(&series_url, "Wikidata")?;
        let series_label = series_data
            .entities
            .get(&series_id)
            .and_then(|entity| entity.labels.get("en"))
            .map(|label| label.value.clone())
            .unwrap_or_else(|| series_id.clone());
        let provenance = Provenance {
            source: "Wikidata".to_string(),
            record_id: qid.to_string(),
            url: format!("https://www.wikidata.org/wiki/{qid}"),
        };

        Ok(Some(WikidataSeries {
            series: ProvenancedValue {
                value: series_label,
                confidence: Confidence::High,
                provenance: provenance.clone(),
            },
            series_index: ordinal.map(|ordinal| ProvenancedValue {
                value: ordinal,
                confidence: Confidence::High,
                provenance,
            }),
        }))
    }
}

#[derive(Debug, Deserialize)]
pub struct EntityData {
    entities: HashMap<String, Entity>,
}

#[derive(Debug, Deserialize)]
struct Entity {
    #[serde(default)]
    labels: HashMap<String, Label>,
    #[serde(default)]
    claims: HashMap<String, Vec<Claim>>,
}

#[derive(Debug, Deserialize)]
struct Label {
    value: String,
}

#[derive(Debug, Deserialize)]
struct Claim {
    mainsnak: Snak,
    #[serde(default)]
    qualifiers: Option<HashMap<String, Vec<Snak>>>,
}

#[derive(Debug, Deserialize)]
struct Snak {
    #[serde(default)]
    datavalue: Option<DataValue>,
}

#[derive(Debug, Deserialize)]
struct DataValue {
    value: serde_json::Value,
}

pub fn extract_series_from_entities(
    data: &EntityData,
    qid: &str,
) -> Option<(String, Option<String>)> {
    let entity = data.entities.get(qid)?;
    let claim = entity.claims.get(PROPERTY_PART_OF_SERIES)?.first()?;
    let series_id = claim_entity_id(claim)?;
    let ordinal = claim
        .qualifiers
        .as_ref()
        .and_then(|qualifiers| qualifiers.get(PROPERTY_SERIES_ORDINAL))
        .and_then(|values| values.first())
        .and_then(claim_string_value);
    let label = data
        .entities
        .get(&series_id)
        .and_then(|entity| entity.labels.get("en"))
        .map(|label| label.value.clone())
        .unwrap_or(series_id);

    Some((label, ordinal))
}

fn claim_entity_id(claim: &Claim) -> Option<String> {
    claim
        .mainsnak
        .datavalue
        .as_ref()?
        .value
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn claim_string_value(snak: &Snak) -> Option<String> {
    snak.datavalue.as_ref()?.value.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{extract_series_from_entities, EntityData};

    #[test]
    fn extracts_wikidata_series_and_ordinal() {
        let data: EntityData = serde_json::from_str(
            r#"{
              "entities": {
                "Q2136877": {
                  "labels": {"en": {"value": "The Way of Kings"}},
                  "claims": {
                    "P179": [{
                      "mainsnak": {"datavalue": {"value": {"id": "Q7766706"}}},
                      "qualifiers": {"P1545": [{"datavalue": {"value": "1"}}]}
                    }]
                  }
                },
                "Q7766706": {
                  "labels": {"en": {"value": "The Stormlight Archive"}},
                  "claims": {}
                }
              }
            }"#,
        )
        .expect("fixture parses");

        assert_eq!(
            extract_series_from_entities(&data, "Q2136877"),
            Some(("The Stormlight Archive".to_string(), Some("1".to_string())))
        );
    }
}
