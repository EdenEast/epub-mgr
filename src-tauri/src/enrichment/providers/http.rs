use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;

use crate::enrichment::ProviderError;

const USER_AGENT: &str = "epub-mgr/0.1 metadata enrichment (https://github.com/EdenEast/epub-mgr)";
const CACHE_ENV_VAR: &str = "EPUB_MGR_METADATA_CACHE";

pub struct HttpJsonClient {
    cache: Mutex<HashMap<String, serde_json::Value>>,
    sqlite: Mutex<SqliteCacheState>,
}

enum SqliteCacheState {
    Uninitialized(PathBuf),
    Ready(Connection),
}

impl Default for HttpJsonClient {
    fn default() -> Self {
        Self::with_cache_path(default_cache_path())
    }
}

impl HttpJsonClient {
    pub fn with_cache_path(path: PathBuf) -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            sqlite: Mutex::new(SqliteCacheState::Uninitialized(path)),
        }
    }

    pub fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        source: &str,
    ) -> Result<T, ProviderError> {
        self.get_json_with_fetcher(url, source, || fetch_json(url, source))
    }

    fn get_json_with_fetcher<T: for<'de> Deserialize<'de>, F>(
        &self,
        url: &str,
        source: &str,
        fetcher: F,
    ) -> Result<T, ProviderError>
    where
        F: FnOnce() -> Result<serde_json::Value, ProviderError>,
    {
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

        if let Ok(Some(cached)) = self.get_cached_json(url, source) {
            if let Ok(parsed) = serde_json::from_value(cached.clone()) {
                self.cache
                    .lock()
                    .map_err(|error| ProviderError::new(source, error.to_string()))?
                    .insert(url.to_string(), cached);
                return Ok(parsed);
            }
        }

        let value = fetcher()?;
        self.cache
            .lock()
            .map_err(|error| ProviderError::new(source, error.to_string()))?
            .insert(url.to_string(), value.clone());
        let _ = self.put_cached_json(url, &value, source);

        serde_json::from_value(value).map_err(|error| ProviderError::new(source, error.to_string()))
    }

    fn get_cached_json(
        &self,
        url: &str,
        source: &str,
    ) -> Result<Option<serde_json::Value>, ProviderError> {
        let body = self.with_sqlite_connection(source, |connection| {
            connection
                .query_row(
                    "SELECT body FROM metadata_http_cache WHERE url = ?1",
                    params![url],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| ProviderError::new(source, error.to_string()))
        })?;

        body.map(|body| serde_json::from_str(&body))
            .transpose()
            .map_err(|error| ProviderError::new(source, error.to_string()))
    }

    fn put_cached_json(
        &self,
        url: &str,
        value: &serde_json::Value,
        source: &str,
    ) -> Result<(), ProviderError> {
        let body = serde_json::to_string(value)
            .map_err(|error| ProviderError::new(source, error.to_string()))?;
        self.with_sqlite_connection(source, |connection| {
            connection
                .execute(
                    "INSERT INTO metadata_http_cache (url, body) VALUES (?1, ?2) \
                     ON CONFLICT(url) DO UPDATE SET body = excluded.body",
                    params![url, body],
                )
                .map(|_| ())
                .map_err(|error| ProviderError::new(source, error.to_string()))
        })
    }

    fn with_sqlite_connection<T>(
        &self,
        source: &str,
        action: impl FnOnce(&Connection) -> Result<T, ProviderError>,
    ) -> Result<T, ProviderError> {
        let mut state = self
            .sqlite
            .lock()
            .map_err(|error| ProviderError::new(source, error.to_string()))?;

        let path = match &*state {
            SqliteCacheState::Uninitialized(path) => Some(path.clone()),
            SqliteCacheState::Ready(_) => None,
        };

        if let Some(path) = path {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)
                    .map_err(|error| ProviderError::new(source, error.to_string()))?;
            }
            let connection = Connection::open(&path)
                .map_err(|error| ProviderError::new(source, error.to_string()))?;
            initialize_cache_schema(&connection, source)?;
            *state = SqliteCacheState::Ready(connection);
        }

        match &*state {
            SqliteCacheState::Uninitialized(_) => unreachable!("sqlite cache not initialized"),
            SqliteCacheState::Ready(connection) => action(connection),
        }
    }
}

fn initialize_cache_schema(connection: &Connection, source: &str) -> Result<(), ProviderError> {
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS metadata_http_cache (
                url TEXT PRIMARY KEY,
                body TEXT NOT NULL
            )",
            [],
        )
        .map_err(|error| ProviderError::new(source, error.to_string()))?;
    Ok(())
}

fn fetch_json(url: &str, source: &str) -> Result<serde_json::Value, ProviderError> {
    let response = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|error| ProviderError::new(source, error.to_string()))?
        .get(url)
        .send()
        .map_err(|error| ProviderError::new(source, error.to_string()))?
        .error_for_status()
        .map_err(|error| ProviderError::new(source, error.to_string()))?;

    response
        .json::<serde_json::Value>()
        .map_err(|error| ProviderError::new(source, error.to_string()))
}

fn default_cache_path() -> PathBuf {
    if let Ok(path) = env::var(CACHE_ENV_VAR) {
        return PathBuf::from(path);
    }

    cache_home()
        .unwrap_or_else(env::temp_dir)
        .join("epub-mgr")
        .join("metadata-cache.sqlite3")
}

fn cache_home() -> Option<PathBuf> {
    env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))
}

#[cfg(test)]
mod tests {
    use super::HttpJsonClient;

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Fixture {
        title: String,
    }

    #[test]
    fn returns_sqlite_cached_response_before_fetching() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_path = temp.path().join("metadata-cache.sqlite3");
        let url = "https://example.test/book.json";

        let first_client = HttpJsonClient::with_cache_path(cache_path.clone());
        let fetched: Fixture = first_client
            .get_json_with_fetcher(url, "Test", || {
                Ok(serde_json::json!({ "title": "cached title" }))
            })
            .expect("initial fetch stores cache");
        assert_eq!(fetched.title, "cached title");

        let second_client = HttpJsonClient::with_cache_path(cache_path);
        let cached: Fixture = second_client
            .get_json_with_fetcher(url, "Test", || {
                panic!("web fetch should not run on cache hit")
            })
            .expect("cache hit deserializes");

        assert_eq!(cached.title, "cached title");
    }
}
