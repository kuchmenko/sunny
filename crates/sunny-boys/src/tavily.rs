//! Tavily web search tool.
//!
//! Provides web search via the [Tavily Search API](https://docs.tavily.com/).
//! Reads `TAVILY_API_KEY` from the environment at search time.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use sunny_core::tool::ToolError;

const TAVILY_API_URL: &str = "https://api.tavily.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Web search tool using the Tavily Search API.
pub struct TavilySearch {
    client: Client,
    base_url: String,
}

#[derive(Serialize)]
struct SearchRequest<'a> {
    api_key: &'a str,
    query: &'a str,
    max_results: usize,
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    title: String,
    url: String,
    content: String,
}

impl TavilySearch {
    /// Creates a new `TavilySearch` configured for the production Tavily API.
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                // Invariant: building a client with only a timeout set cannot fail
                // unless the TLS backend is broken at process level.
                .expect("reqwest client build with timeout-only config is infallible"),
            base_url: TAVILY_API_URL.to_string(),
        }
    }

    /// Creates a `TavilySearch` pointing at a custom base URL (for tests).
    #[cfg(test)]
    fn with_base_url(base_url: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("reqwest client build with timeout-only config is infallible"),
            base_url: base_url.to_string(),
        }
    }

    /// Searches the web via Tavily API.
    ///
    /// Reads `TAVILY_API_KEY` from the environment. Returns formatted results
    /// as `"Title: …\nURL: …\nContent: …"` blocks separated by `---`.
    pub async fn search(&self, query: &str, max_results: usize) -> Result<String, ToolError> {
        let api_key = std::env::var("TAVILY_API_KEY").map_err(|_| ToolError::ExecutionFailed {
            source: "TAVILY_API_KEY not set".into(),
        })?;
        self.execute_search(&api_key, query, max_results).await
    }

    /// Inner search implementation that accepts an explicit API key.
    async fn execute_search(
        &self,
        api_key: &str,
        query: &str,
        max_results: usize,
    ) -> Result<String, ToolError> {
        let url = format!("{}/search", self.base_url);
        let body = SearchRequest {
            api_key,
            query,
            max_results,
        };

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                source: Box::new(e),
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ToolError::ExecutionFailed {
                source: "invalid Tavily API key".into(),
            });
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ToolError::ExecutionFailed {
                source: "rate limit exceeded".into(),
            });
        }
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed {
                source: format!("Tavily API error: {status}").into(),
            });
        }

        let search_res: SearchResponse =
            response
                .json()
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    source: Box::new(e),
                })?;

        if search_res.results.is_empty() {
            return Ok("No results found.".to_string());
        }

        let formatted = search_res
            .results
            .iter()
            .map(|r| format!("Title: {}\nURL: {}\nContent: {}", r.title, r.url, r.content))
            .collect::<Vec<_>>()
            .join("\n---\n");

        Ok(formatted)
    }
}

impl Default for TavilySearch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_tavily_search_with_mock_returns_formatted_results() {
        let mock_server = MockServer::start().await;
        let body = serde_json::json!({
            "results": [
                {
                    "title": "Rust Programming",
                    "url": "https://www.rust-lang.org",
                    "content": "A systems programming language."
                },
                {
                    "title": "Rust Book",
                    "url": "https://doc.rust-lang.org/book/",
                    "content": "The official Rust book."
                }
            ]
        });

        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&mock_server)
            .await;

        let search = TavilySearch::with_base_url(&mock_server.uri());
        let result = search
            .execute_search("test-key", "rust programming", 5)
            .await;

        let output = result.expect("search should succeed");
        assert!(output.contains("Title: Rust Programming"));
        assert!(output.contains("URL: https://www.rust-lang.org"));
        assert!(output.contains("A systems programming language."));
        assert!(output.contains("Title: Rust Book"));
        assert!(output.contains("---"));
    }

    #[tokio::test]
    async fn test_tavily_missing_api_key_returns_error() {
        let saved = std::env::var("TAVILY_API_KEY").ok();
        std::env::remove_var("TAVILY_API_KEY");

        let search = TavilySearch::new();
        let result = search.search("test query", 5).await;

        if let Some(key) = saved {
            std::env::set_var("TAVILY_API_KEY", key);
        }

        let err = result.expect_err("should fail without API key");
        match &err {
            ToolError::ExecutionFailed { source } => {
                assert!(
                    source.to_string().contains("TAVILY_API_KEY not set"),
                    "unexpected source: {source}"
                );
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_tavily_network_timeout_returns_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(15)))
            .mount(&mock_server)
            .await;

        let search = TavilySearch::with_base_url(&mock_server.uri());
        let result = search.execute_search("test-key", "slow query", 5).await;

        let err = result.expect_err("should timeout");
        match &err {
            ToolError::ExecutionFailed { source } => {
                // Walk the error chain to find timeout indication.
                // reqwest wraps the timeout in its source chain.
                let mut full_msg = source.to_string();
                let mut current: &dyn std::error::Error = source.as_ref();
                while let Some(next) = current.source() {
                    full_msg.push_str(&format!(" -> {next}"));
                    current = next;
                }
                assert!(
                    full_msg.contains("timed out")
                        || full_msg.contains("timeout")
                        || full_msg.contains("Timeout"),
                    "expected timeout error in chain, got: {full_msg}"
                );
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_tavily_empty_results_returns_no_results_message() {
        let mock_server = MockServer::start().await;
        let body = serde_json::json!({ "results": [] });

        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&mock_server)
            .await;

        let search = TavilySearch::with_base_url(&mock_server.uri());
        let result = search.execute_search("test-key", "obscure query", 5).await;

        let output = result.expect("empty results should not error");
        assert_eq!(output, "No results found.");
    }

    #[tokio::test]
    async fn test_tavily_error_response_returns_appropriate_error() {
        let mock_server = MockServer::start().await;

        // 401 Unauthorized
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&mock_server)
            .await;

        let search = TavilySearch::with_base_url(&mock_server.uri());
        let result = search.execute_search("bad-key", "query", 5).await;

        let err = result.expect_err("should fail on 401");
        match &err {
            ToolError::ExecutionFailed { source } => {
                assert!(
                    source.to_string().contains("invalid Tavily API key"),
                    "unexpected 401 source: {source}"
                );
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }

        // Reset mock server for 429 test
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let search = TavilySearch::with_base_url(&mock_server.uri());
        let result = search.execute_search("test-key", "query", 5).await;

        let err = result.expect_err("should fail on 429");
        match &err {
            ToolError::ExecutionFailed { source } => {
                assert!(
                    source.to_string().contains("rate limit exceeded"),
                    "unexpected 429 source: {source}"
                );
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }
}
