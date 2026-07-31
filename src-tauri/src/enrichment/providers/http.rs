use std::{collections::HashMap, sync::Mutex};

use serde::Deserialize;

use crate::enrichment::ProviderError;

const USER_AGENT: &str = "epub-mgr/0.1 metadata enrichment (https://github.com/EdenEast/epub-mgr)";

#[derive(Default)]
pub struct HttpJsonClient {
    cache: Mutex<HashMap<String, serde_json::Value>>,
}

impl HttpJsonClient {
    pub fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        source: &str,
    ) -> Result<T, ProviderError> {
        if let Some(cached) = self
            .cache
            .lock()
            .map_err(|error| ProviderError::new(source, error.to_string()))?
            .get(url)
            .cloned()
        {
            return serde_json::from_value(cached)
                .map_err(|error| ProviderError::new(source, error.to_string()));
        }

        let response = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| ProviderError::new(source, error.to_string()))?
            .get(url)
            .send()
            .map_err(|error| ProviderError::new(source, error.to_string()))?
            .error_for_status()
            .map_err(|error| ProviderError::new(source, error.to_string()))?;

        let value = response
            .json::<serde_json::Value>()
            .map_err(|error| ProviderError::new(source, error.to_string()))?;
        self.cache
            .lock()
            .map_err(|error| ProviderError::new(source, error.to_string()))?
            .insert(url.to_string(), value.clone());

        serde_json::from_value(value).map_err(|error| ProviderError::new(source, error.to_string()))
    }
}
