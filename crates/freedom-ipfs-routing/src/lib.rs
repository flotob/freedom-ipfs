use cid::Cid;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

pub const DEFAULT_DELEGATED_ROUTER: &str = "https://delegated-ipfs.dev/routing/v1";

#[derive(Debug, Error)]
pub enum RoutingError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid router response: {0}")]
    InvalidResponse(String),
    #[error("invalid provider url: {0}")]
    InvalidProviderUrl(String),
}

pub type Result<T> = std::result::Result<T, RoutingError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub id: Option<String>,
    pub addrs: Vec<String>,
    pub http_urls: Vec<Url>,
}

#[derive(Debug, Clone)]
pub struct DelegatedRoutingClient {
    endpoint: String,
    client: reqwest::Client,
}

impl Default for DelegatedRoutingClient {
    fn default() -> Self {
        Self::new(DEFAULT_DELEGATED_ROUTER)
    }
}

impl DelegatedRoutingClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn providers(&self, cid: &Cid) -> Result<Vec<Provider>> {
        let url = format!("{}/providers/{}", self.endpoint, cid);
        let body = self
            .client
            .get(url)
            .header("accept", "application/x-ndjson, application/json")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        parse_provider_response(&body)
    }
}

pub fn parse_provider_response(body: &str) -> Result<Vec<Provider>> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    if trimmed.starts_with('{') && trimmed.contains("\"Providers\"") {
        if let Ok(response) = serde_json::from_str::<ProvidersResponse>(trimmed) {
            return response.into_providers();
        }
    }

    let mut providers = Vec::new();
    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Ok(provider) = serde_json::from_str::<ProviderRecord>(line) {
            if provider.id.is_some() || provider.addrs.is_some() {
                providers.push(provider.into_provider()?);
                continue;
            }
        }
        let response: ProvidersResponse = serde_json::from_str(line)
            .map_err(|err| RoutingError::InvalidResponse(err.to_string()))?;
        providers.extend(response.into_providers()?);
    }
    Ok(providers)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ProvidersResponse {
    providers: Option<Vec<ProviderRecord>>,
}

impl ProvidersResponse {
    fn into_providers(self) -> Result<Vec<Provider>> {
        self.providers
            .unwrap_or_default()
            .into_iter()
            .map(ProviderRecord::into_provider)
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ProviderRecord {
    id: Option<String>,
    addrs: Option<Vec<String>>,
}

impl ProviderRecord {
    fn into_provider(self) -> Result<Provider> {
        let addrs = self.addrs.unwrap_or_default();
        let mut http_urls = Vec::new();
        for addr in &addrs {
            if let Some(url) = http_url_from_multiaddr(addr)? {
                http_urls.push(url);
            }
        }
        Ok(Provider {
            id: self.id,
            addrs,
            http_urls,
        })
    }
}

fn http_url_from_multiaddr(addr: &str) -> Result<Option<Url>> {
    let parts: Vec<&str> = addr.split('/').filter(|part| !part.is_empty()).collect();
    let Some(http_pos) = parts
        .iter()
        .position(|part| *part == "http" || *part == "https")
    else {
        return Ok(None);
    };
    let scheme =
        if parts[http_pos] == "https" || parts.get(http_pos.wrapping_sub(1)) == Some(&"tls") {
            "https"
        } else {
            "http"
        };

    let host = parts
        .windows(2)
        .find_map(|pair| match pair[0] {
            "dns" | "dns4" | "dns6" | "ip4" | "ip6" => Some(pair[1]),
            _ => None,
        })
        .ok_or_else(|| RoutingError::InvalidProviderUrl(addr.to_string()))?;
    let port = parts.windows(2).find_map(|pair| {
        if pair[0] == "tcp" {
            Some(pair[1])
        } else {
            None
        }
    });

    let url = if let Some(port) = port {
        format!("{scheme}://{host}:{port}")
    } else {
        format!("{scheme}://{host}")
    };
    Url::parse(&url)
        .map(Some)
        .map_err(|_| RoutingError::InvalidProviderUrl(addr.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_tls_http_provider_urls() {
        let body =
            r#"{"Providers":[{"ID":"peer","Addrs":["/dns4/example.com/tcp/443/tls/http"]}]}"#;
        let providers = parse_provider_response(body).unwrap();
        assert_eq!(providers[0].http_urls[0].as_str(), "https://example.com/");
    }
}
