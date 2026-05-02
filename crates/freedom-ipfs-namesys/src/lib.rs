use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NamesysError {
    #[error("dnslink record not found for {0}")]
    NotFound(String),
    #[error("invalid dnslink record: {0}")]
    InvalidDnslink(String),
    #[error("http resolver: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, NamesysError>;

#[async_trait]
pub trait DnsTxtResolver: Send + Sync {
    async fn txt_lookup(&self, name: &str) -> Result<Vec<String>>;
}

#[derive(Debug, Clone)]
pub struct CloudflareDohResolver {
    client: reqwest::Client,
    endpoint: String,
}

impl Default for CloudflareDohResolver {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: "https://cloudflare-dns.com/dns-query".to_string(),
        }
    }
}

impl CloudflareDohResolver {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            ..Self::default()
        }
    }
}

#[async_trait]
impl DnsTxtResolver for CloudflareDohResolver {
    async fn txt_lookup(&self, name: &str) -> Result<Vec<String>> {
        let response = self
            .client
            .get(&self.endpoint)
            .query(&[("name", name), ("type", "TXT")])
            .header("accept", "application/dns-json")
            .send()
            .await?
            .error_for_status()?
            .json::<DohResponse>()
            .await?;

        Ok(response
            .answer
            .unwrap_or_default()
            .into_iter()
            .map(|answer| unquote_txt(&answer.data))
            .collect())
    }
}

pub async fn resolve_dnslink(resolver: &dyn DnsTxtResolver, domain: &str) -> Result<String> {
    let lookup = if domain.starts_with("_dnslink.") {
        domain.to_string()
    } else {
        format!("_dnslink.{domain}")
    };
    let records = resolver.txt_lookup(&lookup).await?;
    for record in records {
        if let Some(value) = parse_dnslink_txt(&record)? {
            return Ok(value);
        }
    }
    Err(NamesysError::NotFound(domain.to_string()))
}

pub fn parse_dnslink_txt(record: &str) -> Result<Option<String>> {
    let record = record.trim();
    let Some(value) = record.strip_prefix("dnslink=") else {
        return Ok(None);
    };
    if value.starts_with("/ipfs/") || value.starts_with("/ipns/") {
        Ok(Some(value.to_string()))
    } else {
        Err(NamesysError::InvalidDnslink(value.to_string()))
    }
}

fn unquote_txt(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].replace("\\\"", "\"")
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct DohResponse {
    #[serde(rename = "Answer")]
    answer: Option<Vec<DohAnswer>>,
}

#[derive(Debug, Deserialize)]
struct DohAnswer {
    data: String,
}

#[derive(Debug, Clone)]
pub struct IpnsRecord {
    pub value: String,
    pub sequence: u64,
    pub validity: Option<String>,
}

pub fn verify_ipns_record_placeholder(record: IpnsRecord) -> Result<IpnsRecord> {
    if record.value.starts_with("/ipfs/") || record.value.starts_with("/ipns/") {
        Ok(record)
    } else {
        Err(NamesysError::InvalidDnslink(record.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dnslink_records() {
        assert_eq!(
            parse_dnslink_txt("dnslink=/ipfs/bafyexample").unwrap(),
            Some("/ipfs/bafyexample".to_string())
        );
        assert_eq!(parse_dnslink_txt("not-dnslink").unwrap(), None);
        assert!(parse_dnslink_txt("dnslink=https://example.com").is_err());
    }
}
