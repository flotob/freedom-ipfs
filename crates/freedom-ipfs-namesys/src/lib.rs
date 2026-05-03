use async_trait::async_trait;
use cid::Cid;
use ipld_core::ipld::Ipld;
use libp2p_identity::{PeerId, PublicKey};
use multihash::Multihash;
use prost::Message;
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const DEFAULT_DELEGATED_ROUTER: &str = "https://delegated-ipfs.dev/routing/v1";
const IPNS_RECORD_CONTENT_TYPE: &str = "application/vnd.ipfs.ipns-record";
const IPNS_SIGNATURE_PREFIX: &[u8] = b"ipns-signature:";
const IPNS_RECORD_MAX_SIZE: usize = 10 * 1024;
const LIBP2P_KEY_CODEC: u64 = 0x72;
const IDENTITY_HASH: u64 = 0x00;
const DEFAULT_NAME_CACHE_TTL: Duration = Duration::from_secs(60);
const DEFAULT_NAMESYS_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum NamesysError {
    #[error("dnslink record not found for {0}")]
    NotFound(String),
    #[error("invalid dnslink record: {0}")]
    InvalidDnslink(String),
    #[error("invalid IPNS name: {0}")]
    InvalidIpnsName(String),
    #[error("invalid IPNS record: {0}")]
    InvalidIpnsRecord(String),
    #[error("expired IPNS record")]
    ExpiredIpnsRecord,
    #[error("IPNS signature verification failed")]
    InvalidIpnsSignature,
    #[error("http resolver: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, NamesysError>;

#[async_trait]
pub trait DnsTxtResolver: Send + Sync {
    async fn txt_lookup(&self, name: &str) -> Result<Vec<String>>;
}

#[async_trait]
pub trait IpnsResolver: Send + Sync {
    async fn resolve_ipns(&self, name: &str) -> Result<IpnsRecord>;
}

#[async_trait]
impl<T> IpnsResolver for Arc<T>
where
    T: IpnsResolver + ?Sized,
{
    async fn resolve_ipns(&self, name: &str) -> Result<IpnsRecord> {
        self.as_ref().resolve_ipns(name).await
    }
}

#[async_trait]
pub trait NameResolver: Send + Sync {
    async fn resolve_name(&self, name: &str) -> Result<String>;
}

#[derive(Debug, Clone)]
pub struct CachedNameResolver<R> {
    inner: R,
    ttl: Duration,
    cache: Arc<Mutex<HashMap<String, CachedName>>>,
}

impl<R> CachedNameResolver<R> {
    pub fn new(inner: R) -> Self {
        Self::with_ttl(inner, DEFAULT_NAME_CACHE_TTL)
    }

    pub fn with_ttl(inner: R, ttl: Duration) -> Self {
        Self {
            inner,
            ttl,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl<R> NameResolver for CachedNameResolver<R>
where
    R: NameResolver,
{
    async fn resolve_name(&self, name: &str) -> Result<String> {
        if let Some(value) = self.cached(name) {
            return Ok(value);
        }

        let value = self.inner.resolve_name(name).await?;
        self.store(name, &value);
        Ok(value)
    }
}

impl<R> CachedNameResolver<R> {
    fn cached(&self, name: &str) -> Option<String> {
        let mut cache = self.cache.lock().ok()?;
        let entry = cache.get(name)?;
        if entry.expires_at > Instant::now() {
            return Some(entry.value.clone());
        }
        cache.remove(name);
        None
    }

    fn store(&self, name: &str, value: &str) {
        if self.ttl.is_zero() {
            return;
        }
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                name.to_string(),
                CachedName {
                    value: value.to_string(),
                    expires_at: Instant::now() + self.ttl,
                },
            );
        }
    }
}

#[derive(Debug, Clone)]
struct CachedName {
    value: String,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct CloudflareDohResolver {
    client: reqwest::Client,
    endpoint: String,
}

impl Default for CloudflareDohResolver {
    fn default() -> Self {
        Self {
            client: timeout_http_client(DEFAULT_NAMESYS_HTTP_TIMEOUT),
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

#[derive(Debug, Clone)]
pub struct DelegatedIpnsResolver {
    endpoint: String,
    client: reqwest::Client,
}

impl Default for DelegatedIpnsResolver {
    fn default() -> Self {
        Self::new(DEFAULT_DELEGATED_ROUTER)
    }
}

impl DelegatedIpnsResolver {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            client: timeout_http_client(DEFAULT_NAMESYS_HTTP_TIMEOUT),
        }
    }
}

#[async_trait]
impl IpnsResolver for DelegatedIpnsResolver {
    async fn resolve_ipns(&self, name: &str) -> Result<IpnsRecord> {
        let lookup = normalize_ipns_name_for_routing(name)?;
        let url = format!("{}/ipns/{lookup}", self.endpoint);
        let response = self
            .client
            .get(url)
            .header("accept", IPNS_RECORD_CONTENT_TYPE)
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(NamesysError::NotFound(name.to_string()));
        }
        let response = response.error_for_status()?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if content_type != IPNS_RECORD_CONTENT_TYPE {
            return Err(NamesysError::NotFound(name.to_string()));
        }

        let bytes = response.bytes().await?;
        verify_ipns_record(name, &bytes)
    }
}

#[derive(Debug, Clone, Default)]
pub struct DefaultNameResolver<I = DelegatedIpnsResolver> {
    dnslink: CloudflareDohResolver,
    ipns: I,
}

impl<I> DefaultNameResolver<I> {
    pub fn new(dnslink: CloudflareDohResolver, ipns: I) -> Self {
        Self { dnslink, ipns }
    }
}

#[async_trait]
impl<I> NameResolver for DefaultNameResolver<I>
where
    I: IpnsResolver,
{
    async fn resolve_name(&self, name: &str) -> Result<String> {
        match resolve_dnslink(&self.dnslink, name).await {
            Ok(path) => Ok(path),
            Err(NamesysError::NotFound(_)) if is_ipns_name(name) => self
                .ipns
                .resolve_ipns(name)
                .await
                .map(|record| record.value),
            Err(err) => Err(err),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FallbackIpnsResolver<P, F> {
    primary: P,
    fallback: F,
}

impl<P, F> FallbackIpnsResolver<P, F> {
    pub fn new(primary: P, fallback: F) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl<P, F> IpnsResolver for FallbackIpnsResolver<P, F>
where
    P: IpnsResolver,
    F: IpnsResolver,
{
    async fn resolve_ipns(&self, name: &str) -> Result<IpnsRecord> {
        let primary_err = match self.primary.resolve_ipns(name).await {
            Ok(record) => return Ok(record),
            Err(err) => err,
        };

        match self.fallback.resolve_ipns(name).await {
            Ok(record) => Ok(record),
            Err(_) => Err(primary_err),
        }
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

pub fn is_ipns_name(name: &str) -> bool {
    parse_ipns_name(name).is_ok()
}

pub fn verify_ipns_record(name: &str, bytes: &[u8]) -> Result<IpnsRecord> {
    verify_ipns_record_at(name, bytes, OffsetDateTime::now_utc())
}

fn verify_ipns_record_at(name: &str, bytes: &[u8], now: OffsetDateTime) -> Result<IpnsRecord> {
    if bytes.len() > IPNS_RECORD_MAX_SIZE {
        return Err(NamesysError::InvalidIpnsRecord(format!(
            "record exceeds {IPNS_RECORD_MAX_SIZE} byte limit"
        )));
    }
    let name = parse_ipns_name(name)?;
    let entry = IpnsEntry::decode(bytes)
        .map_err(|err| NamesysError::InvalidIpnsRecord(format!("protobuf decode failed: {err}")))?;
    let signature = entry
        .signature_v2
        .as_deref()
        .filter(|signature| !signature.is_empty())
        .ok_or_else(|| NamesysError::InvalidIpnsRecord("missing signatureV2".into()))?;
    let data = entry
        .data
        .as_deref()
        .filter(|data| !data.is_empty())
        .ok_or_else(|| NamesysError::InvalidIpnsRecord("missing data".into()))?;

    let public_key = extract_public_key(&name, entry.pub_key.as_deref())?;
    if public_key.to_peer_id() != name.peer_id {
        return Err(NamesysError::InvalidIpnsRecord(
            "public key does not match IPNS name".into(),
        ));
    }

    let record_data = parse_ipns_data(data)?;
    let mut signed = Vec::with_capacity(IPNS_SIGNATURE_PREFIX.len() + data.len());
    signed.extend_from_slice(IPNS_SIGNATURE_PREFIX);
    signed.extend_from_slice(data);
    if !public_key.verify(&signed, signature) {
        return Err(NamesysError::InvalidIpnsSignature);
    }

    verify_legacy_fields_match(&entry, &record_data)?;
    if record_data.validity_type != 0 {
        return Err(NamesysError::InvalidIpnsRecord(
            "unsupported validity type".into(),
        ));
    }
    let validity = std::str::from_utf8(&record_data.validity)
        .map_err(|_| NamesysError::InvalidIpnsRecord("validity is not utf-8".into()))?;
    let expires = OffsetDateTime::parse(validity, &Rfc3339)
        .map_err(|err| NamesysError::InvalidIpnsRecord(format!("invalid validity: {err}")))?;
    if expires <= now {
        return Err(NamesysError::ExpiredIpnsRecord);
    }

    let value = String::from_utf8(record_data.value.clone())
        .map_err(|_| NamesysError::InvalidIpnsRecord("value is not utf-8".into()))?;
    if !value.starts_with("/ipfs/") && !value.starts_with("/ipns/") {
        return Err(NamesysError::InvalidIpnsRecord(format!(
            "unsupported value path: {value}"
        )));
    }

    Ok(IpnsRecord {
        value,
        sequence: record_data.sequence,
        validity: Some(validity.to_string()),
        ttl: record_data.ttl,
    })
}

fn normalize_ipns_name_for_routing(name: &str) -> Result<String> {
    let cid = ipns_name_as_cid(name)?;
    Ok(cid.to_string())
}

pub fn ipns_dht_record_key(name: &str) -> Result<Vec<u8>> {
    let mut key = b"/ipns/".to_vec();
    key.extend_from_slice(&ipns_name_as_cid(name)?.hash().to_bytes());
    Ok(key)
}

fn ipns_name_as_cid(name: &str) -> Result<Cid> {
    if let Ok(cid) = name.parse::<Cid>() {
        if cid.codec() == LIBP2P_KEY_CODEC {
            return Ok(cid);
        }
    }
    let peer_id =
        PeerId::from_str(name).map_err(|err| NamesysError::InvalidIpnsName(err.to_string()))?;
    let hash = Multihash::<64>::from_bytes(&peer_id.to_bytes())
        .map_err(|err| NamesysError::InvalidIpnsName(err.to_string()))?;
    Ok(Cid::new_v1(LIBP2P_KEY_CODEC, hash))
}

fn parse_ipns_name(name: &str) -> Result<IpnsName> {
    if let Ok(cid) = name.parse::<Cid>() {
        if cid.codec() != LIBP2P_KEY_CODEC {
            return Err(NamesysError::InvalidIpnsName(format!(
                "expected libp2p-key codec, got {}",
                cid.codec()
            )));
        }
        let hash = cid.hash();
        let peer_id = PeerId::from_bytes(&hash.to_bytes())
            .map_err(|err| NamesysError::InvalidIpnsName(err.to_string()))?;
        let inline_public_key = (hash.code() == IDENTITY_HASH).then(|| hash.digest().to_vec());
        return Ok(IpnsName {
            peer_id,
            inline_public_key,
        });
    }

    let peer_id =
        PeerId::from_str(name).map_err(|err| NamesysError::InvalidIpnsName(err.to_string()))?;
    let hash = Multihash::<64>::from_bytes(&peer_id.to_bytes())
        .map_err(|err| NamesysError::InvalidIpnsName(err.to_string()))?;
    Ok(IpnsName {
        peer_id,
        inline_public_key: (hash.code() == IDENTITY_HASH).then(|| hash.digest().to_vec()),
    })
}

fn extract_public_key(name: &IpnsName, record_key: Option<&[u8]>) -> Result<PublicKey> {
    if let Some(record_key) = record_key {
        return PublicKey::try_decode_protobuf(record_key).map_err(|err| {
            NamesysError::InvalidIpnsRecord(format!("public key decode failed: {err}"))
        });
    }
    let inline = name.inline_public_key.as_deref().ok_or_else(|| {
        NamesysError::InvalidIpnsRecord("record omitted public key for hashed IPNS name".into())
    })?;
    PublicKey::try_decode_protobuf(inline).map_err(|err| {
        NamesysError::InvalidIpnsRecord(format!("inline public key decode failed: {err}"))
    })
}

fn parse_ipns_data(data: &[u8]) -> Result<IpnsRecordData> {
    let value: Ipld = serde_ipld_dagcbor::from_slice(data)
        .map_err(|err| NamesysError::InvalidIpnsRecord(format!("data decode failed: {err}")))?;
    let Ipld::Map(map) = value else {
        return Err(NamesysError::InvalidIpnsRecord(
            "data must be a DAG-CBOR map".into(),
        ));
    };
    Ok(IpnsRecordData {
        value: required_bytes(&map, "Value")?,
        validity: required_bytes(&map, "Validity")?,
        validity_type: required_u64(&map, "ValidityType")?,
        sequence: required_u64(&map, "Sequence")?,
        ttl: required_u64(&map, "TTL")?,
    })
}

fn verify_legacy_fields_match(entry: &IpnsEntry, data: &IpnsRecordData) -> Result<()> {
    if let Some(value) = &entry.value {
        if value != &data.value {
            return Err(NamesysError::InvalidIpnsRecord(
                "legacy value does not match signed data".into(),
            ));
        }
    }
    if let Some(validity) = &entry.validity {
        if validity != &data.validity {
            return Err(NamesysError::InvalidIpnsRecord(
                "legacy validity does not match signed data".into(),
            ));
        }
    }
    if let Some(validity_type) = entry.validity_type {
        if validity_type as u64 != data.validity_type {
            return Err(NamesysError::InvalidIpnsRecord(
                "legacy validityType does not match signed data".into(),
            ));
        }
    }
    if let Some(sequence) = entry.sequence {
        if sequence != data.sequence {
            return Err(NamesysError::InvalidIpnsRecord(
                "legacy sequence does not match signed data".into(),
            ));
        }
    }
    if let Some(ttl) = entry.ttl {
        if ttl != data.ttl {
            return Err(NamesysError::InvalidIpnsRecord(
                "legacy ttl does not match signed data".into(),
            ));
        }
    }
    Ok(())
}

fn required_bytes(map: &std::collections::BTreeMap<String, Ipld>, key: &str) -> Result<Vec<u8>> {
    match map.get(key) {
        Some(Ipld::Bytes(bytes)) => Ok(bytes.clone()),
        Some(_) => Err(NamesysError::InvalidIpnsRecord(format!(
            "{key} must be bytes"
        ))),
        None => Err(NamesysError::InvalidIpnsRecord(format!("missing {key}"))),
    }
}

fn required_u64(map: &std::collections::BTreeMap<String, Ipld>, key: &str) -> Result<u64> {
    match map.get(key) {
        Some(Ipld::Integer(value)) if *value >= 0 && *value <= u64::MAX as i128 => {
            Ok(*value as u64)
        }
        Some(_) => Err(NamesysError::InvalidIpnsRecord(format!(
            "{key} must be a non-negative integer"
        ))),
        None => Err(NamesysError::InvalidIpnsRecord(format!("missing {key}"))),
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

fn timeout_http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .build()
        .expect("namesystem HTTP client config is valid")
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpnsRecord {
    pub value: String,
    pub sequence: u64,
    pub validity: Option<String>,
    pub ttl: u64,
}

pub fn verify_ipns_record_placeholder(record: IpnsRecord) -> Result<IpnsRecord> {
    if record.value.starts_with("/ipfs/") || record.value.starts_with("/ipns/") {
        Ok(record)
    } else {
        Err(NamesysError::InvalidDnslink(record.value))
    }
}

#[derive(Debug)]
struct IpnsName {
    peer_id: PeerId,
    inline_public_key: Option<Vec<u8>>,
}

#[derive(Debug)]
struct IpnsRecordData {
    value: Vec<u8>,
    validity: Vec<u8>,
    validity_type: u64,
    sequence: u64,
    ttl: u64,
}

#[derive(Clone, PartialEq, Message)]
struct IpnsEntry {
    #[prost(bytes = "vec", optional, tag = "1")]
    value: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    signature_v1: Option<Vec<u8>>,
    #[prost(enumeration = "ValidityType", optional, tag = "3")]
    validity_type: Option<i32>,
    #[prost(bytes = "vec", optional, tag = "4")]
    validity: Option<Vec<u8>>,
    #[prost(uint64, optional, tag = "5")]
    sequence: Option<u64>,
    #[prost(uint64, optional, tag = "6")]
    ttl: Option<u64>,
    #[prost(bytes = "vec", optional, tag = "7")]
    pub_key: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "8")]
    signature_v2: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "9")]
    data: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
enum ValidityType {
    Eol = 0,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipld_core::ipld::Ipld;
    use libp2p_identity::Keypair;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FUTURE: &str = "2126-01-01T00:00:00.000000000Z";
    const PAST: &str = "2020-01-01T00:00:00.000000000Z";

    #[test]
    fn parses_dnslink_records() {
        assert_eq!(
            parse_dnslink_txt("dnslink=/ipfs/bafyexample").unwrap(),
            Some("/ipfs/bafyexample".to_string())
        );
        assert_eq!(parse_dnslink_txt("not-dnslink").unwrap(), None);
        assert!(parse_dnslink_txt("dnslink=https://example.com").is_err());
    }

    #[tokio::test]
    async fn cached_name_resolver_reuses_successful_resolution() {
        let count = Arc::new(AtomicUsize::new(0));
        let resolver = CachedNameResolver::with_ttl(
            CountingNameResolver {
                count: count.clone(),
            },
            Duration::from_secs(60),
        );

        assert_eq!(
            resolver.resolve_name("example.test").await.unwrap(),
            "/ipfs/1"
        );
        assert_eq!(
            resolver.resolve_name("example.test").await.unwrap(),
            "/ipfs/1"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fallback_ipns_resolver_uses_fallback_after_primary_failure() {
        let resolver = FallbackIpnsResolver::new(
            StaticIpnsResolver { record: None },
            StaticIpnsResolver {
                record: Some(IpnsRecord {
                    value: "/ipfs/bafkqaddwgevxmmraojswg33smq".to_string(),
                    sequence: 42,
                    validity: None,
                    ttl: 0,
                }),
            },
        );

        let record = resolver.resolve_ipns("k51fallback").await.unwrap();

        assert_eq!(record.sequence, 42);
    }

    #[test]
    fn verifies_v2_ipns_record_with_inline_ed25519_key() {
        let (name, record) = signed_record("/ipfs/bafkqaddwgevxmmraojswg33smq", FUTURE, true);
        let verified = verify_ipns_record(&name, &record).unwrap();
        assert_eq!(verified.value, "/ipfs/bafkqaddwgevxmmraojswg33smq");
        assert_eq!(verified.sequence, 7);
        assert_eq!(verified.ttl, 300_000_000_000);
    }

    #[test]
    fn rejects_tampered_ipns_signature() {
        let (name, mut record) = signed_record("/ipfs/bafkqaddwgevxmmraojswg33smq", FUTURE, true);
        let last = record.last_mut().unwrap();
        *last ^= 0x01;
        assert!(matches!(
            verify_ipns_record(&name, &record),
            Err(NamesysError::InvalidIpnsSignature) | Err(NamesysError::InvalidIpnsRecord(_))
        ));
    }

    #[test]
    fn rejects_expired_ipns_record() {
        let (name, record) = signed_record("/ipfs/bafkqaddwgevxmmraojswg33smq", PAST, true);
        assert!(matches!(
            verify_ipns_record(&name, &record),
            Err(NamesysError::ExpiredIpnsRecord)
        ));
    }

    #[test]
    fn rejects_legacy_field_mismatch() {
        let (name, record) = signed_record_with_legacy_value(
            "/ipfs/bafkqaddwgevxmmraojswg33smq",
            "/ipfs/bafkqahtwgevxmmrao5uxi2bamjzg623fnyqhg2lhnzqxi5lsmuqhmmi",
            FUTURE,
        );
        assert!(matches!(
            verify_ipns_record(&name, &record),
            Err(NamesysError::InvalidIpnsRecord(_))
        ));
    }

    #[test]
    fn verifies_v2_only_ipns_record() {
        let (name, record) = signed_record("/ipfs/bafkqaddwgevxmmraojswg33smq", FUTURE, false);
        let verified = verify_ipns_record(&name, &record).unwrap();
        assert_eq!(verified.value, "/ipfs/bafkqaddwgevxmmraojswg33smq");
    }

    #[test]
    fn builds_binary_ipns_dht_record_key() {
        let (name, _record) = signed_record("/ipfs/bafkqaddwgevxmmraojswg33smq", FUTURE, false);
        let key = ipns_dht_record_key(&name).unwrap();

        assert!(key.starts_with(b"/ipns/"));
        assert_eq!(
            &key[6..],
            ipns_name_as_cid(&name).unwrap().hash().to_bytes()
        );
    }

    fn signed_record(value: &str, validity: &str, include_legacy: bool) -> (String, Vec<u8>) {
        signed_record_inner(value, value, validity, include_legacy)
    }

    fn signed_record_with_legacy_value(
        value: &str,
        legacy_value: &str,
        validity: &str,
    ) -> (String, Vec<u8>) {
        signed_record_inner(value, legacy_value, validity, true)
    }

    fn signed_record_inner(
        value: &str,
        legacy_value: &str,
        validity: &str,
        include_legacy: bool,
    ) -> (String, Vec<u8>) {
        let keypair = Keypair::generate_ed25519();
        let public = keypair.public();
        let name = Cid::new_v1(
            LIBP2P_KEY_CODEC,
            Multihash::<64>::from_bytes(&public.to_peer_id().to_bytes()).unwrap(),
        )
        .to_string();
        let data = ipns_data(value, validity);
        let mut signed = IPNS_SIGNATURE_PREFIX.to_vec();
        signed.extend_from_slice(&data);
        let signature = keypair.sign(&signed).unwrap();

        let entry = IpnsEntry {
            value: include_legacy.then(|| legacy_value.as_bytes().to_vec()),
            signature_v1: None,
            validity_type: include_legacy.then_some(ValidityType::Eol as i32),
            validity: include_legacy.then(|| validity.as_bytes().to_vec()),
            sequence: include_legacy.then_some(7),
            ttl: include_legacy.then_some(300_000_000_000),
            pub_key: None,
            signature_v2: Some(signature),
            data: Some(data),
        };
        (name, entry.encode_to_vec())
    }

    fn ipns_data(value: &str, validity: &str) -> Vec<u8> {
        let mut map = BTreeMap::new();
        map.insert("Sequence".to_string(), Ipld::Integer(7));
        map.insert("TTL".to_string(), Ipld::Integer(300_000_000_000));
        map.insert(
            "Validity".to_string(),
            Ipld::Bytes(validity.as_bytes().to_vec()),
        );
        map.insert("ValidityType".to_string(), Ipld::Integer(0));
        map.insert("Value".to_string(), Ipld::Bytes(value.as_bytes().to_vec()));
        serde_ipld_dagcbor::to_vec(&Ipld::Map(map)).unwrap()
    }

    struct CountingNameResolver {
        count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl NameResolver for CountingNameResolver {
        async fn resolve_name(&self, _name: &str) -> Result<String> {
            let value = self.count.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("/ipfs/{value}"))
        }
    }

    struct StaticIpnsResolver {
        record: Option<IpnsRecord>,
    }

    #[async_trait]
    impl IpnsResolver for StaticIpnsResolver {
        async fn resolve_ipns(&self, _name: &str) -> Result<IpnsRecord> {
            self.record
                .clone()
                .ok_or_else(|| NamesysError::NotFound("static".into()))
        }
    }
}
