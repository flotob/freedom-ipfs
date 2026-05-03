use cid::Cid;
use freedom_ipfs_namesys::{
    ipns_dht_record_key, verify_ipns_record, IpnsRecord, IpnsResolver, NamesysError,
};
use futures::StreamExt;
use libp2p::kad::{
    self, store::MemoryStore, store::RecordStore, GetProvidersOk, GetRecordError, GetRecordOk,
    QueryResult,
};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::SwarmEvent;
use libp2p::{noise, tcp, tls, yamux, Multiaddr, PeerId, SwarmBuilder};
use serde::Deserialize;
use std::collections::HashSet;
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;
use url::Url;

pub const DEFAULT_DELEGATED_ROUTER: &str = "https://delegated-ipfs.dev/routing/v1";
pub const DEFAULT_DHT_QUERY_TIMEOUT: Duration = Duration::from_secs(25);
pub const DEFAULT_MAX_DHT_PROVIDERS: usize = 32;
const DEFAULT_DELEGATED_ROUTING_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_BOOTSTRAP_PEERS: &[&str] = &[
    "/dnsaddr/sg1.bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
    "/dnsaddr/sv15.bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "/dnsaddr/am6.bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    "/dnsaddr/ny5.bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "/dnsaddr/va1.bootstrap.libp2p.io/p2p/12D3KooWKnDdG3iXw9eTFijk3EWSunZcFi54Zka4wmtqtt6rPxc8",
];

#[derive(Debug, Error)]
pub enum RoutingError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid router response: {0}")]
    InvalidResponse(String),
    #[error("invalid provider url: {0}")]
    InvalidProviderUrl(String),
    #[error("dht: {0}")]
    Dht(String),
}

pub type Result<T> = std::result::Result<T, RoutingError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub id: Option<String>,
    pub addrs: Vec<String>,
    pub http_urls: Vec<Url>,
}

impl Provider {
    pub fn from_parts(id: Option<String>, addrs: Vec<String>) -> Result<Self> {
        let mut http_urls = Vec::new();
        for addr in &addrs {
            if let Some(url) = http_url_from_multiaddr(addr)? {
                http_urls.push(url);
            }
        }
        Ok(Self {
            id,
            addrs,
            http_urls,
        })
    }
}

#[derive(Debug, Clone)]
pub enum ProviderRoutingClient {
    Delegated(DelegatedRoutingClient),
    Auto(AutoRoutingClient),
    LightDht(LightDhtClient),
}

impl ProviderRoutingClient {
    pub async fn providers(&self, cid: &Cid) -> Result<Vec<Provider>> {
        match self {
            Self::Delegated(client) => client.providers(cid).await,
            Self::Auto(client) => client.providers(cid).await,
            Self::LightDht(client) => client.providers(cid).await,
        }
    }
}

impl From<DelegatedRoutingClient> for ProviderRoutingClient {
    fn from(client: DelegatedRoutingClient) -> Self {
        Self::Delegated(client)
    }
}

impl From<AutoRoutingClient> for ProviderRoutingClient {
    fn from(client: AutoRoutingClient) -> Self {
        Self::Auto(client)
    }
}

impl From<LightDhtClient> for ProviderRoutingClient {
    fn from(client: LightDhtClient) -> Self {
        Self::LightDht(client)
    }
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
            client: timeout_http_client(DEFAULT_DELEGATED_ROUTING_TIMEOUT),
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

#[derive(Debug, Clone)]
pub struct AutoRoutingClient {
    delegated: DelegatedRoutingClient,
    dht: LightDhtClient,
}

impl Default for AutoRoutingClient {
    fn default() -> Self {
        Self::new(DelegatedRoutingClient::default(), LightDhtClient::default())
    }
}

impl AutoRoutingClient {
    pub fn new(delegated: DelegatedRoutingClient, dht: LightDhtClient) -> Self {
        Self { delegated, dht }
    }

    pub async fn providers(&self, cid: &Cid) -> Result<Vec<Provider>> {
        match self.delegated.providers(cid).await {
            Ok(providers) if !providers.is_empty() => Ok(providers),
            Ok(_) => self.dht.providers(cid).await,
            Err(delegated_err) => match self.dht.providers(cid).await {
                Ok(providers) => Ok(providers),
                Err(dht_err) => Err(RoutingError::Dht(format!(
                    "delegated routing failed ({delegated_err}); light DHT failed ({dht_err})"
                ))),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct LightDhtClient {
    bootstrap_peers: Vec<String>,
    query_timeout: Duration,
    max_providers: usize,
}

impl Default for LightDhtClient {
    fn default() -> Self {
        Self {
            bootstrap_peers: DEFAULT_BOOTSTRAP_PEERS
                .iter()
                .map(|peer| (*peer).to_string())
                .collect(),
            query_timeout: DEFAULT_DHT_QUERY_TIMEOUT,
            max_providers: DEFAULT_MAX_DHT_PROVIDERS,
        }
    }
}

impl LightDhtClient {
    pub fn new(bootstrap_peers: Vec<String>) -> Self {
        Self {
            bootstrap_peers,
            ..Self::default()
        }
    }

    pub fn with_query_timeout(mut self, timeout: Duration) -> Self {
        self.query_timeout = if timeout.is_zero() {
            Duration::from_secs(1)
        } else {
            timeout
        };
        self
    }

    pub fn with_max_providers(mut self, max_providers: usize) -> Self {
        self.max_providers = max_providers.max(1);
        self
    }

    pub async fn providers(&self, cid: &Cid) -> Result<Vec<Provider>> {
        let mut swarm = self.bootstrapped_swarm().await?;

        let key = kad::RecordKey::new(&cid.hash().to_bytes());
        let query_id = swarm.behaviour_mut().get_providers(key.clone());
        let mut provider_ids = HashSet::new();
        let deadline = tokio::time::sleep(self.query_timeout);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = &mut deadline => {
                    break;
                }
                event = swarm.select_next_some() => {
                    let SwarmEvent::Behaviour(kad::Event::OutboundQueryProgressed { id, result, .. }) = event else {
                        continue;
                    };
                    if id != query_id {
                        continue;
                    }
                    match result {
                        QueryResult::GetProviders(Ok(GetProvidersOk::FoundProviders { providers, .. })) => {
                            provider_ids.extend(providers);
                            if provider_ids.len() >= self.max_providers {
                                break;
                            }
                        }
                        QueryResult::GetProviders(Ok(GetProvidersOk::FinishedWithNoAdditionalRecord { .. })) => {
                            break;
                        }
                        QueryResult::GetProviders(Err(err)) => {
                            if provider_ids.is_empty() {
                                return Err(RoutingError::Dht(err.to_string()));
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        let providers = providers_from_dht(&mut swarm, &key, provider_ids, self.max_providers)?;
        resolve_missing_provider_addresses(&mut swarm, providers, self.query_timeout).await
    }

    pub async fn records(&self, key: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut swarm = self.bootstrapped_swarm().await?;
        let key = kad::RecordKey::new(&key.to_vec());
        let query_id = swarm.behaviour_mut().get_record(key);
        let deadline = tokio::time::sleep(self.query_timeout);
        tokio::pin!(deadline);
        let mut records = Vec::new();

        loop {
            tokio::select! {
                _ = &mut deadline => {
                    if records.is_empty() {
                        return Err(RoutingError::Dht("DHT record lookup timed out".into()));
                    }
                    break;
                }
                event = swarm.select_next_some() => {
                    let SwarmEvent::Behaviour(kad::Event::OutboundQueryProgressed { id, result, .. }) = event else {
                        continue;
                    };
                    if id != query_id {
                        continue;
                    }
                    match result {
                        QueryResult::GetRecord(Ok(GetRecordOk::FoundRecord(record))) => {
                            records.push(record.record.value);
                        }
                        QueryResult::GetRecord(Ok(GetRecordOk::FinishedWithNoAdditionalRecord { .. })) => {
                            break;
                        }
                        QueryResult::GetRecord(Err(GetRecordError::QuorumFailed { records: found_records, .. })) => {
                            records.extend(found_records.into_iter().map(|record| record.record.value));
                            break;
                        }
                        QueryResult::GetRecord(Err(GetRecordError::NotFound { .. })) => {
                            break;
                        }
                        QueryResult::GetRecord(Err(err)) => {
                            if records.is_empty() {
                                return Err(RoutingError::Dht(err.to_string()));
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(records)
    }

    async fn bootstrapped_swarm(&self) -> Result<libp2p::Swarm<kad::Behaviour<MemoryStore>>> {
        let mut swarm = build_dht_swarm(self.query_timeout).await?;
        let mut bootstrap_count = 0usize;
        for addr in &self.bootstrap_peers {
            let Some((peer, addr)) = parse_p2p_multiaddr(addr) else {
                tracing::debug!(addr, "ignoring invalid DHT bootstrap peer");
                continue;
            };
            swarm.behaviour_mut().add_address(&peer, addr.clone());
            swarm.add_peer_address(peer, addr.clone());
            if let Ok(dial_addr) = addr.with_p2p(peer) {
                if let Err(err) = swarm.dial(dial_addr) {
                    tracing::debug!(peer = %peer, error = %err, "DHT bootstrap dial rejected");
                }
            }
            bootstrap_count += 1;
        }
        if bootstrap_count == 0 {
            return Err(RoutingError::Dht("no valid DHT bootstrap peers".into()));
        }
        Ok(swarm)
    }
}

#[derive(Debug, Clone)]
pub struct DhtIpnsResolver {
    dht: LightDhtClient,
}

impl Default for DhtIpnsResolver {
    fn default() -> Self {
        Self::new(LightDhtClient::default())
    }
}

impl DhtIpnsResolver {
    pub fn new(dht: LightDhtClient) -> Self {
        Self { dht }
    }
}

#[async_trait::async_trait]
impl IpnsResolver for DhtIpnsResolver {
    async fn resolve_ipns(&self, name: &str) -> freedom_ipfs_namesys::Result<IpnsRecord> {
        let key = ipns_dht_record_key(name)?;
        let records =
            self.dht.records(&key).await.map_err(|err| {
                NamesysError::NotFound(format!("DHT IPNS record for {name}: {err}"))
            })?;

        let mut best = None;
        for record in records {
            let Ok(record) = verify_ipns_record(name, &record) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|best: &IpnsRecord| record.sequence > best.sequence)
            {
                best = Some(record);
            }
        }

        best.ok_or_else(|| NamesysError::NotFound(name.to_string()))
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
    #[serde(rename = "ID", alias = "Id")]
    id: Option<String>,
    addrs: Option<Vec<String>>,
}

impl ProviderRecord {
    fn into_provider(self) -> Result<Provider> {
        Provider::from_parts(self.id, self.addrs.unwrap_or_default())
    }
}

async fn build_dht_swarm(
    query_timeout: Duration,
) -> Result<libp2p::Swarm<kad::Behaviour<MemoryStore>>> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            (tls::Config::new, noise::Config::new),
            yamux::Config::default,
        )
        .map_err(|err| RoutingError::Dht(err.to_string()))?
        .with_quic()
        .with_dns()
        .map_err(|err| RoutingError::Dht(err.to_string()))?
        .with_websocket(
            (tls::Config::new, noise::Config::new),
            yamux::Config::default,
        )
        .await
        .map_err(|err| RoutingError::Dht(err.to_string()))?
        .with_behaviour(move |key| {
            let peer_id = key.public().to_peer_id();
            let store = MemoryStore::new(peer_id);
            let mut config = kad::Config::new(kad::PROTOCOL_NAME);
            config.set_query_timeout(query_timeout);
            config.set_periodic_bootstrap_interval(None);
            let mut behaviour = kad::Behaviour::with_config(peer_id, store, config);
            behaviour.set_mode(Some(kad::Mode::Client));
            behaviour
        })
        .map_err(|err| RoutingError::Dht(err.to_string()))
        .map(|builder| {
            builder
                .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(20)))
                .build()
        })
}

fn providers_from_dht(
    swarm: &mut libp2p::Swarm<kad::Behaviour<MemoryStore>>,
    key: &kad::RecordKey,
    provider_ids: HashSet<PeerId>,
    max_providers: usize,
) -> Result<Vec<Provider>> {
    let records = swarm.behaviour_mut().store_mut().providers(key);
    let mut providers = Vec::new();
    for peer_id in provider_ids.into_iter().take(max_providers) {
        let mut addrs = records
            .iter()
            .filter(|record| record.provider == peer_id)
            .flat_map(|record| record.addresses.iter().cloned())
            .collect::<Vec<_>>();
        if addrs.is_empty() {
            addrs = peer_addresses_from_kbuckets(swarm.behaviour_mut(), &peer_id);
        }
        providers.push(Provider::from_parts(
            Some(peer_id.to_string()),
            addrs.into_iter().map(|addr| addr.to_string()).collect(),
        )?);
    }
    Ok(providers)
}

async fn resolve_missing_provider_addresses(
    swarm: &mut libp2p::Swarm<kad::Behaviour<MemoryStore>>,
    mut providers: Vec<Provider>,
    query_timeout: Duration,
) -> Result<Vec<Provider>> {
    let mut pending = Vec::new();
    for (index, provider) in providers.iter().enumerate() {
        if !provider.addrs.is_empty() {
            continue;
        }
        let Some(peer) = provider.id.as_deref().and_then(parse_peer_id) else {
            continue;
        };
        let query_id = swarm.behaviour_mut().get_closest_peers(peer.to_bytes());
        pending.push((query_id, peer, index));
    }

    if pending.is_empty() {
        providers.retain(|provider| !provider.addrs.is_empty());
        return Ok(providers);
    }

    let deadline = tokio::time::sleep(query_timeout);
    tokio::pin!(deadline);
    while !pending.is_empty() {
        tokio::select! {
            _ = &mut deadline => break,
            event = swarm.select_next_some() => {
                let SwarmEvent::Behaviour(kad::Event::OutboundQueryProgressed { id, result, .. }) = event else {
                    continue;
                };
                let Some(pos) = pending.iter().position(|(query_id, _, _)| *query_id == id) else {
                    continue;
                };
                let (_, target_peer, provider_index) = pending.swap_remove(pos);
                let peer_info = match result {
                    QueryResult::GetClosestPeers(Ok(ok)) => ok
                        .peers
                        .into_iter()
                        .find(|peer| peer.peer_id == target_peer),
                    QueryResult::GetClosestPeers(Err(kad::GetClosestPeersError::Timeout { peers, .. })) => peers
                        .into_iter()
                        .find(|peer| peer.peer_id == target_peer),
                    _ => None,
                };
                let Some(peer_info) = peer_info else {
                    continue;
                };
                providers[provider_index] = Provider::from_parts(
                    Some(target_peer.to_string()),
                    peer_info
                        .addrs
                        .into_iter()
                        .map(|addr| addr.to_string())
                        .collect(),
                )?;
            }
        }
    }

    providers.retain(|provider| !provider.addrs.is_empty());
    Ok(providers)
}

fn peer_addresses_from_kbuckets(
    behaviour: &mut kad::Behaviour<MemoryStore>,
    peer_id: &PeerId,
) -> Vec<Multiaddr> {
    let mut addrs = Vec::new();
    for bucket in behaviour.kbuckets() {
        for entry in bucket.iter() {
            if entry.node.key.preimage() == peer_id {
                addrs.extend(entry.node.value.iter().cloned());
            }
        }
    }
    addrs
}

fn parse_peer_id(id: &str) -> Option<PeerId> {
    PeerId::from_str(id).ok()
}

fn parse_p2p_multiaddr(addr: &str) -> Option<(PeerId, Multiaddr)> {
    let mut multiaddr = Multiaddr::from_str(addr).ok()?;
    let peer = match multiaddr.iter().last()? {
        Protocol::P2p(peer) => peer,
        _ => return None,
    };
    multiaddr.pop();
    Some((peer, multiaddr))
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

fn timeout_http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .build()
        .expect("delegated routing HTTP client config is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_tls_http_provider_urls() {
        let body =
            r#"{"Providers":[{"ID":"peer","Addrs":["/dns4/example.com/tcp/443/tls/http"]}]}"#;
        let providers = parse_provider_response(body).unwrap();
        assert_eq!(providers[0].id.as_deref(), Some("peer"));
        assert_eq!(providers[0].http_urls[0].as_str(), "https://example.com/");
    }

    #[test]
    fn parses_ndjson_peer_records_with_uppercase_id() {
        let body = r#"{"Addrs":["/ip4/164.92.225.198/tcp/4001"],"ID":"12D3KooWNDpFqyse9kR7aZwgEzh4U1mL6Zz6jEuRNFXJxL5D2KPP","Schema":"peer"}"#;
        let providers = parse_provider_response(body).unwrap();
        assert_eq!(
            providers[0].id.as_deref(),
            Some("12D3KooWNDpFqyse9kR7aZwgEzh4U1mL6Zz6jEuRNFXJxL5D2KPP")
        );
        assert_eq!(providers[0].addrs[0], "/ip4/164.92.225.198/tcp/4001");
    }

    #[test]
    fn parses_dht_bootstrap_multiaddr() {
        let (peer, addr) = parse_p2p_multiaddr(
            "/dnsaddr/ny5.bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
        )
        .unwrap();
        assert_eq!(
            peer.to_string(),
            "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa"
        );
        assert_eq!(addr.to_string(), "/dnsaddr/ny5.bootstrap.libp2p.io");
    }

    #[test]
    fn configures_max_dht_providers_with_floor() {
        assert_eq!(
            LightDhtClient::default()
                .with_max_providers(4)
                .max_providers,
            4
        );
        assert_eq!(
            LightDhtClient::default()
                .with_max_providers(0)
                .max_providers,
            1
        );
    }

    #[test]
    fn configures_dht_query_timeout_with_floor() {
        assert_eq!(
            LightDhtClient::default()
                .with_query_timeout(Duration::from_secs(7))
                .query_timeout,
            Duration::from_secs(7)
        );
        assert_eq!(
            LightDhtClient::default()
                .with_query_timeout(Duration::ZERO)
                .query_timeout,
            Duration::from_secs(1)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn light_dht_finds_provider_from_local_server_peer() {
        let cid = "bafybeiaql2jo3fu5b7c4lmpoi5drh5sam7yt652shwdgwbky4o7uw33u2u"
            .parse::<Cid>()
            .unwrap();
        let (peer_id, addr, swarm_task) = spawn_local_dht_provider(cid).await;
        let bootstrap = format!("{addr}/p2p/{peer_id}");

        let providers = LightDhtClient::new(vec![bootstrap])
            .with_query_timeout(Duration::from_secs(5))
            .with_max_providers(1)
            .providers(&cid)
            .await
            .unwrap();

        let expected_peer_id = peer_id.to_string();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id.as_deref(), Some(expected_peer_id.as_str()));
        assert!(providers[0].addrs.iter().any(|provider_addr| {
            provider_addr == &addr.to_string() || provider_addr.starts_with("/ip4/127.0.0.1/tcp/")
        }));
        swarm_task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "network smoke test against the public Amino DHT"]
    async fn live_light_dht_finds_public_providers() {
        let cid = std::env::var("FREEDOM_IPFS_LIVE_DHT_CID")
            .unwrap_or_else(|_| {
                "bafybeiaql2jo3fu5b7c4lmpoi5drh5sam7yt652shwdgwbky4o7uw33u2u".into()
            })
            .parse::<Cid>()
            .unwrap();
        let providers = LightDhtClient::default()
            .with_query_timeout(Duration::from_secs(30))
            .providers(&cid)
            .await
            .unwrap();
        eprintln!("DHT found {} providers for {cid}", providers.len());
        for provider in &providers {
            eprintln!(
                "provider {} addrs={:?}",
                provider.id.as_deref().unwrap_or("<unknown>"),
                provider.addrs
            );
        }
        assert!(!providers.is_empty());
    }

    async fn spawn_local_dht_provider(
        cid: Cid,
    ) -> (PeerId, Multiaddr, tokio::task::JoinHandle<()>) {
        let mut swarm = build_local_dht_server().await;
        let peer_id = *swarm.local_peer_id();
        swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let addr = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                break address;
            }
        };
        let key = kad::RecordKey::new(&cid.hash().to_bytes());
        let provider = kad::ProviderRecord::new(key, peer_id, vec![addr.clone()]);
        swarm
            .behaviour_mut()
            .store_mut()
            .add_provider(provider)
            .unwrap();

        let task = tokio::spawn(async move {
            loop {
                let _ = swarm.select_next_some().await;
            }
        });
        (peer_id, addr, task)
    }

    async fn build_local_dht_server() -> libp2p::Swarm<kad::Behaviour<MemoryStore>> {
        SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                (tls::Config::new, noise::Config::new),
                yamux::Config::default,
            )
            .unwrap()
            .with_behaviour(|key| {
                let peer_id = key.public().to_peer_id();
                let store = MemoryStore::new(peer_id);
                let mut config = kad::Config::new(kad::PROTOCOL_NAME);
                config.set_query_timeout(Duration::from_secs(5));
                config.set_periodic_bootstrap_interval(None);
                let mut behaviour = kad::Behaviour::with_config(peer_id, store, config);
                behaviour.set_mode(Some(kad::Mode::Server));
                behaviour
            })
            .unwrap()
            .build()
    }
}
