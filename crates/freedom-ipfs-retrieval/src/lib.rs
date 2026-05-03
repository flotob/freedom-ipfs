use cid::Cid;
use freedom_ipfs_core::{
    verify_block, Block, BlockProvider, CoreError, Result as CoreResult, CODEC_DAG_PB,
    HASH_IDENTITY, HASH_SHA2_256,
};
use freedom_ipfs_namesys::{CloudflareDohResolver, DnsTxtResolver};
use freedom_ipfs_routing::{Provider, ProviderRoutingClient};
use freedom_ipfs_store::{CachedProviderRecord, SqliteBlockStore};
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use futures::stream::{select_all, FuturesUnordered};
use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::StreamProtocol;
use libp2p::{noise, tcp, tls, yamux, Multiaddr, PeerId, SwarmBuilder};
use libp2p_stream::{Control as StreamControl, IncomingStreams};
use multihash::Multihash;
use multihash_codetable::{Code, MultihashDigest};
use prost::Message;
use std::collections::BTreeSet;
use std::io;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::timeout;
use url::Url;

const PROVIDER_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const BAD_HTTP_PROVIDER_TTL: Duration = Duration::from_secs(10 * 60);
const BAD_BITSWAP_PROVIDER_TTL: Duration = Duration::from_secs(2 * 60);
const MAX_BITSWAP_PEERS_PER_BLOCK: usize = 16;
const MAX_BITSWAP_ADDRS_PER_PEER: usize = 4;
const CID_VERSION_0: u64 = 0;
const CID_VERSION_1: u64 = 1;

#[derive(Debug, Error)]
pub enum RetrievalError {
    #[error("routing: {0}")]
    Routing(#[from] freedom_ipfs_routing::RoutingError),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("core: {0}")]
    Core(#[from] freedom_ipfs_core::CoreError),
    #[error("store: {0}")]
    Store(#[from] freedom_ipfs_store::StoreError),
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    #[error("bitswap: {0}")]
    Bitswap(String),
    #[error("bitswap request timed out")]
    BitswapTimeout,
    #[error("no HTTP-capable providers found")]
    NoHttpProviders,
    #[error("no Bitswap-capable providers found")]
    NoBitswapProviders,
}

pub type Result<T> = std::result::Result<T, RetrievalError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalSource {
    Cache,
    HttpProvider,
    Bitswap,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetrievalStats {
    pub cache_hits: u64,
    pub http_provider_blocks: u64,
    pub bitswap_blocks: u64,
}

#[derive(Default)]
struct RetrievalStatsInner {
    cache_hits: AtomicU64,
    http_provider_blocks: AtomicU64,
    bitswap_blocks: AtomicU64,
}

impl RetrievalStatsInner {
    fn record(&self, source: RetrievalSource) {
        match source {
            RetrievalSource::Cache => &self.cache_hits,
            RetrievalSource::HttpProvider => &self.http_provider_blocks,
            RetrievalSource::Bitswap => &self.bitswap_blocks,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> RetrievalStats {
        RetrievalStats {
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            http_provider_blocks: self.http_provider_blocks.load(Ordering::Relaxed),
            bitswap_blocks: self.bitswap_blocks.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
pub struct HttpRetriever {
    client: reqwest::Client,
    routing: ProviderRoutingClient,
    store: SqliteBlockStore,
}

impl HttpRetriever {
    pub fn new(routing: impl Into<ProviderRoutingClient>, store: SqliteBlockStore) -> Self {
        Self {
            client: reqwest::Client::new(),
            routing: routing.into(),
            store,
        }
    }

    pub async fn fetch_block(&self, cid: &Cid) -> Result<Block> {
        self.fetch_block_with_source(cid)
            .await
            .map(|(block, _source)| block)
    }

    pub async fn fetch_block_with_source(&self, cid: &Cid) -> Result<(Block, RetrievalSource)> {
        if let Some(block) = self.store.get(cid)? {
            return Ok((block, RetrievalSource::Cache));
        }

        let providers = match self.cached_providers(cid)? {
            Some(providers) => providers,
            None => {
                let providers = self.routing.providers(cid).await?;
                self.cache_providers(cid, &providers)?;
                providers
            }
        };
        match self.fetch_from_providers_with_source(cid, &providers).await {
            Ok(block) => Ok(block),
            Err(err) if should_refresh_providers_after_failure(&err) => {
                let refreshed = match self.routing.providers(cid).await {
                    Ok(providers) => providers,
                    Err(refresh_err) => {
                        tracing::debug!(
                            cid = %cid,
                            error = %refresh_err,
                            "provider refresh after retrieval failure failed"
                        );
                        return Err(err);
                    }
                };
                if same_provider_set(&providers, &refreshed) {
                    return Err(err);
                }
                self.cache_providers(cid, &refreshed)?;
                match self.fetch_from_providers_with_source(cid, &refreshed).await {
                    Ok(block) => Ok(block),
                    Err(refresh_err) => Err(RetrievalError::Bitswap(format!(
                        "initial provider retrieval failed ({err}); refreshed provider retrieval failed ({refresh_err})"
                    ))),
                }
            }
            Err(err) => Err(err),
        }
    }

    pub async fn fetch_from_providers(&self, cid: &Cid, providers: &[Provider]) -> Result<Block> {
        self.fetch_from_providers_with_source(cid, providers)
            .await
            .map(|(block, _source)| block)
    }

    pub async fn fetch_from_providers_with_source(
        &self,
        cid: &Cid,
        providers: &[Provider],
    ) -> Result<(Block, RetrievalSource)> {
        for provider in providers {
            for base in &provider.http_urls {
                if self.store.is_bad_provider(base.as_str())? {
                    tracing::debug!(provider = %base, "skipping temporarily bad HTTP provider");
                    continue;
                }
                match self.fetch_from_http_provider(cid, base).await {
                    Ok(block) => return Ok((block, RetrievalSource::HttpProvider)),
                    Err(err) => {
                        let _ = self.store.mark_bad_provider(
                            base.as_str(),
                            &err.to_string(),
                            BAD_HTTP_PROVIDER_TTL,
                        );
                        continue;
                    }
                }
            }
        }
        match self.fetch_from_bitswap_providers(cid, providers).await {
            Ok(block) => Ok((block, RetrievalSource::Bitswap)),
            Err(RetrievalError::NoBitswapProviders) => Err(RetrievalError::NoHttpProviders),
            Err(err) => Err(err),
        }
    }

    fn cached_providers(&self, cid: &Cid) -> Result<Option<Vec<Provider>>> {
        let Some(records) = self.store.get_provider_records(cid)? else {
            return Ok(None);
        };
        let providers = records
            .into_iter()
            .map(|record| Provider::from_parts(record.id, record.addrs))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if providers.is_empty() {
            Ok(None)
        } else {
            Ok(Some(providers))
        }
    }

    fn cache_providers(&self, cid: &Cid, providers: &[Provider]) -> Result<()> {
        let records = providers
            .iter()
            .map(|provider| CachedProviderRecord {
                id: provider.id.clone(),
                addrs: provider.addrs.clone(),
            })
            .collect::<Vec<_>>();
        self.store
            .put_provider_records(cid, &records, PROVIDER_CACHE_TTL)?;
        Ok(())
    }

    async fn fetch_from_http_provider(&self, cid: &Cid, base: &Url) -> Result<Block> {
        let url = base
            .join(&format!("/ipfs/{cid}?format=raw"))
            .map_err(RetrievalError::Url)?;
        let bytes = self
            .client
            .get(url)
            .header("accept", "application/vnd.ipld.raw")
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        verify_block(cid, &bytes)?;
        self.store.put_block(cid, &bytes)?;
        Ok(Block::unchecked(*cid, bytes.to_vec()))
    }

    async fn fetch_from_bitswap_providers(
        &self,
        cid: &Cid,
        providers: &[Provider],
    ) -> Result<Block> {
        let mut peers = bitswap_peers(providers).await;
        peers.retain(
            |peer| match self.store.is_bad_provider(&peer.id.to_string()) {
                Ok(false) => true,
                Ok(true) => {
                    tracing::debug!(peer = %peer.id, "skipping temporarily bad Bitswap provider");
                    false
                }
                Err(_) => true,
            },
        );
        tracing::debug!(
            provider_count = providers.len(),
            peer_count = peers.len(),
            "bitswap provider candidates"
        );
        if peers.is_empty() {
            return Err(RetrievalError::NoBitswapProviders);
        }

        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                (tls::Config::new, noise::Config::new),
                yamux::Config::default,
            )
            .map_err(|err| RetrievalError::Bitswap(err.to_string()))?
            .with_quic()
            .with_dns()
            .map_err(|err| RetrievalError::Bitswap(err.to_string()))?
            .with_websocket(
                (tls::Config::new, noise::Config::new),
                yamux::Config::default,
            )
            .await
            .map_err(|err| RetrievalError::Bitswap(err.to_string()))?
            .with_behaviour(|_| libp2p_stream::Behaviour::new())
            .map_err(|err| RetrievalError::Bitswap(err.to_string()))?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(20)))
            .build();

        let mut control = swarm.behaviour().new_control();
        let incoming = accept_bitswap_streams(&mut control)?;
        let mut peer_ids = Vec::new();
        for peer in peers {
            tracing::debug!(peer = %peer.id, addrs = ?peer.addrs, "adding bitswap peer");
            peer_ids.push(peer.id);
            for addr in peer.addrs {
                swarm.add_peer_address(peer.id, addr.clone());
                let dial_addr = addr.with_p2p(peer.id).unwrap_or_else(|addr| addr);
                if let Err(err) = swarm.dial(dial_addr) {
                    tracing::debug!(peer = %peer.id, error = %err, "bitswap dial rejected");
                }
            }
        }

        let swarm_task = tokio::spawn(async move {
            loop {
                let _ = swarm.select_next_some().await;
            }
        });

        let request_peer_ids = peer_ids.clone();
        let result = match timeout(Duration::from_secs(45), async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            fetch_bitswap_over_streams(control, incoming, request_peer_ids, *cid).await
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                swarm_task.abort();
                for peer_id in &peer_ids {
                    let _ = self.store.mark_bad_provider(
                        &peer_id.to_string(),
                        "bitswap request timed out",
                        BAD_BITSWAP_PROVIDER_TTL,
                    );
                }
                return Err(RetrievalError::BitswapTimeout);
            }
        };
        swarm_task.abort();
        let result = match result {
            Ok(result) => result,
            Err(err) => return Err(err),
        };
        for (extra_cid, extra_data) in &result.extra_blocks {
            if extra_cid != cid {
                let _ = self.store.put_block(extra_cid, extra_data);
            }
        }
        self.store.put_block(cid, &result.requested_block)?;
        Ok(Block::unchecked(*cid, result.requested_block))
    }
}

#[derive(Clone)]
pub struct FetchingBlockProvider {
    store: SqliteBlockStore,
    retriever: HttpRetriever,
    stats: Arc<RetrievalStatsInner>,
}

impl FetchingBlockProvider {
    pub fn new(store: SqliteBlockStore, routing: impl Into<ProviderRoutingClient>) -> Self {
        let retriever = HttpRetriever::new(routing, store.clone());
        Self {
            store,
            retriever,
            stats: Arc::new(RetrievalStatsInner::default()),
        }
    }

    pub fn stats(&self) -> RetrievalStats {
        self.stats.snapshot()
    }
}

impl BlockProvider for FetchingBlockProvider {
    fn get_block(&self, cid: &Cid) -> CoreResult<Option<Block>> {
        if let Some(block) = self
            .store
            .get(cid)
            .map_err(|err| CoreError::Storage(err.to_string()))?
        {
            self.stats.record(RetrievalSource::Cache);
            return Ok(Some(block));
        }

        let fetched = match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| {
                handle.block_on(self.retriever.fetch_block_with_source(cid))
            }),
            Err(_) => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| CoreError::Storage(err.to_string()))?;
                runtime.block_on(self.retriever.fetch_block_with_source(cid))
            }
        };

        match fetched {
            Ok((block, source)) => {
                self.stats.record(source);
                Ok(Some(block))
            }
            Err(err) => Err(CoreError::Storage(err.to_string())),
        }
    }
}

#[derive(Clone)]
struct BitswapPeer {
    id: PeerId,
    addrs: Vec<Multiaddr>,
}

struct BitswapFetchResult {
    requested_block: Vec<u8>,
    extra_blocks: Vec<(Cid, Vec<u8>)>,
}

#[derive(Clone)]
struct ReceivedBitswapBlock {
    cid: Option<Cid>,
    data: Vec<u8>,
}

async fn bitswap_peers(providers: &[Provider]) -> Vec<BitswapPeer> {
    let mut peers = Vec::new();

    for provider in providers {
        let provider_peer = provider.id.as_deref().and_then(parse_peer_id);
        let mut addrs = Vec::new();
        let mut peer_id = provider_peer;

        for addr in expand_dnsaddr_records(&provider.addrs).await {
            let Some((addr_peer, dial_addr)) = parse_bitswap_multiaddr(&addr, provider_peer) else {
                continue;
            };
            peer_id.get_or_insert(addr_peer);
            addrs.push(dial_addr);
        }

        if let Some(id) = peer_id {
            if !addrs.is_empty() {
                addrs.sort_by_key(bitswap_addr_score);
                addrs.dedup();
                addrs.truncate(MAX_BITSWAP_ADDRS_PER_PEER);
                merge_bitswap_peer(&mut peers, id, addrs);
            }
        }
    }

    peers.truncate(MAX_BITSWAP_PEERS_PER_BLOCK);
    peers
}

fn merge_bitswap_peer(peers: &mut Vec<BitswapPeer>, id: PeerId, addrs: Vec<Multiaddr>) {
    if let Some(peer) = peers.iter_mut().find(|peer| peer.id == id) {
        peer.addrs.extend(addrs);
        peer.addrs.sort_by_key(bitswap_addr_score);
        peer.addrs.dedup();
        peer.addrs.truncate(MAX_BITSWAP_ADDRS_PER_PEER);
    } else {
        peers.push(BitswapPeer { id, addrs });
    }
}

async fn expand_dnsaddr_records(addrs: &[String]) -> Vec<String> {
    let resolver = CloudflareDohResolver::default();
    let mut expanded = Vec::new();

    for addr in addrs {
        let Some(host) = dnsaddr_host(addr) else {
            expanded.push(addr.clone());
            continue;
        };
        let lookup = format!("_dnsaddr.{host}");
        let Ok(records) = resolver.txt_lookup(&lookup).await else {
            continue;
        };
        expanded.extend(records.into_iter().filter_map(|record| {
            record
                .trim()
                .strip_prefix("dnsaddr=")
                .map(ToOwned::to_owned)
        }));
    }

    expanded
}

fn dnsaddr_host(addr: &str) -> Option<&str> {
    let mut parts = addr.split('/').filter(|part| !part.is_empty());
    if parts.next()? == "dnsaddr" {
        parts.next()
    } else {
        None
    }
}

fn parse_bitswap_multiaddr(
    addr: &str,
    provider_peer: Option<PeerId>,
) -> Option<(PeerId, Multiaddr)> {
    let mut multiaddr = Multiaddr::from_str(addr).ok()?;
    let addr_peer = match multiaddr.iter().last() {
        Some(Protocol::P2p(peer)) => {
            multiaddr.pop();
            Some(peer)
        }
        _ => None,
    };
    let peer_id = addr_peer.or(provider_peer)?;
    if is_supported_bitswap_addr(&multiaddr) {
        Some((peer_id, multiaddr))
    } else {
        None
    }
}

fn parse_peer_id(id: &str) -> Option<PeerId> {
    PeerId::from_str(id).ok()
}

fn is_supported_bitswap_addr(addr: &Multiaddr) -> bool {
    let mut has_tcp = false;
    let mut has_udp = false;
    let mut has_quic = false;
    for protocol in addr.iter() {
        match protocol {
            Protocol::Tcp(_) => has_tcp = true,
            Protocol::Udp(_) => has_udp = true,
            Protocol::Quic | Protocol::QuicV1 => has_quic = true,
            Protocol::WebTransport
            | Protocol::WebRTC
            | Protocol::WebRTCDirect
            | Protocol::P2pWebRtcDirect
            | Protocol::P2pCircuit
            | Protocol::Certhash(_) => return false,
            _ => {}
        }
    }
    has_tcp || (has_udp && has_quic)
}

fn bitswap_addr_score(addr: &Multiaddr) -> u8 {
    let mut has_ip = false;
    let mut has_dns = false;
    let mut has_tcp = false;
    let mut has_quic = false;
    let mut has_ws = false;

    for protocol in addr.iter() {
        match protocol {
            Protocol::Ip4(_) | Protocol::Ip6(_) => has_ip = true,
            Protocol::Dns(_) | Protocol::Dns4(_) | Protocol::Dns6(_) | Protocol::Dnsaddr(_) => {
                has_dns = true
            }
            Protocol::Tcp(_) => has_tcp = true,
            Protocol::Quic | Protocol::QuicV1 => has_quic = true,
            Protocol::Ws(_) | Protocol::Wss(_) => has_ws = true,
            _ => {}
        }
    }

    match (has_ip, has_dns, has_tcp, has_quic, has_ws) {
        (true, _, true, _, false) => 0,
        (true, _, _, true, false) => 1,
        (_, true, true, _, false) => 2,
        (_, true, _, true, false) => 3,
        (true, _, true, _, true) => 4,
        (_, true, true, _, true) => 5,
        _ => 9,
    }
}

fn should_refresh_providers_after_failure(err: &RetrievalError) -> bool {
    matches!(
        err,
        RetrievalError::Bitswap(_)
            | RetrievalError::BitswapTimeout
            | RetrievalError::NoHttpProviders
            | RetrievalError::NoBitswapProviders
    )
}

fn same_provider_set(left: &[Provider], right: &[Provider]) -> bool {
    normalized_provider_set(left) == normalized_provider_set(right)
}

fn normalized_provider_set(providers: &[Provider]) -> BTreeSet<(Option<String>, Vec<String>)> {
    providers
        .iter()
        .map(|provider| {
            let mut addrs = provider.addrs.clone();
            addrs.sort();
            addrs.dedup();
            (provider.id.clone(), addrs)
        })
        .collect()
}

fn accept_bitswap_streams(control: &mut StreamControl) -> Result<Vec<IncomingStreams>> {
    bitswap_protocols()
        .into_iter()
        .map(|protocol| {
            control
                .accept(protocol)
                .map_err(|err| RetrievalError::Bitswap(err.to_string()))
        })
        .collect()
}

async fn fetch_bitswap_over_streams(
    control: StreamControl,
    incoming: Vec<IncomingStreams>,
    peer_ids: Vec<PeerId>,
    cid: Cid,
) -> Result<BitswapFetchResult> {
    let mut incoming = select_all(incoming);
    let mut attempts = FuturesUnordered::new();
    for peer_id in peer_ids {
        attempts.push(request_bitswap_block(control.clone(), peer_id, cid));
    }

    let mut failures = Vec::new();
    loop {
        tokio::select! {
            maybe_result = attempts.next() => {
                match maybe_result {
                    Some(Ok(data)) => return Ok(data),
                    Some(Err(err)) => failures.push(err),
                    None => {
                        let detail = if failures.is_empty() {
                            "no bitswap request attempts completed".to_string()
                        } else {
                            failures.join("; ")
                        };
                        return Err(RetrievalError::Bitswap(format!(
                            "all bitswap stream requests failed: {detail}"
                        )));
                    }
                }
            }
            maybe_stream = incoming.next() => {
                let Some((_peer, mut stream)) = maybe_stream else {
                    continue;
                };
                match read_bitswap_blocks(&mut stream).await {
                    Ok(blocks) => {
                        if let Some(result) = collect_bitswap_result(&cid, blocks) {
                            let _ = write_bitswap_cancel(&mut stream, &cid).await;
                            return Ok(result);
                        }
                        let _ = write_empty_bitswap_message(&mut stream).await;
                    }
                    Err(err) => failures.push(err.to_string()),
                }
            }
        }
    }
}

async fn request_bitswap_block(
    mut control: StreamControl,
    peer_id: PeerId,
    cid: Cid,
) -> std::result::Result<BitswapFetchResult, String> {
    let mut failures = Vec::new();
    for protocol in bitswap_protocols() {
        let protocol_name = protocol.to_string();
        let stream = timeout(
            Duration::from_secs(10),
            control.open_stream(peer_id, protocol.clone()),
        )
        .await;
        let mut stream = match stream {
            Ok(Ok(stream)) => stream,
            Ok(Err(err)) => {
                failures.push(format!("{protocol_name}: open failed: {err}"));
                continue;
            }
            Err(_) => {
                failures.push(format!("{protocol_name}: open timed out"));
                continue;
            }
        };

        if let Err(err) = write_bitswap_want(&mut stream, &cid).await {
            failures.push(format!("{protocol_name}: write failed: {err}"));
            continue;
        }
        let blocks = match timeout(Duration::from_secs(10), read_bitswap_blocks(&mut stream)).await
        {
            Ok(Ok(blocks)) => blocks,
            Ok(Err(err)) => {
                failures.push(format!("{protocol_name}: read failed: {err}"));
                continue;
            }
            Err(_) => {
                failures.push(format!("{protocol_name}: read timed out"));
                continue;
            }
        };
        if let Some(result) = collect_bitswap_result(&cid, blocks) {
            let _ = write_bitswap_cancel(&mut stream, &cid).await;
            return Ok(result);
        }
        failures.push(format!("{protocol_name}: no valid block returned"));
    }
    Err(format!(
        "{peer_id}: no supported Bitswap protocol returned the requested block ({})",
        failures.join("; ")
    ))
}

fn bitswap_protocols() -> [StreamProtocol; 3] {
    [
        StreamProtocol::new("/ipfs/bitswap/1.2.0"),
        StreamProtocol::new("/ipfs/bitswap/1.1.0"),
        StreamProtocol::new("/ipfs/bitswap/1.0.0"),
    ]
}

async fn write_bitswap_want<T>(io: &mut T, cid: &Cid) -> io::Result<()>
where
    T: AsyncWrite + Unpin,
{
    let message = bitswap_want_message(cid, false);
    write_length_prefixed(io, &message.encode_to_vec()).await?;
    io.flush().await
}

async fn write_bitswap_cancel<T>(io: &mut T, cid: &Cid) -> io::Result<()>
where
    T: AsyncWrite + Unpin,
{
    let message = bitswap_want_message(cid, true);
    write_length_prefixed(io, &message.encode_to_vec()).await?;
    io.flush().await
}

fn bitswap_want_message(cid: &Cid, cancel: bool) -> BitswapMessage {
    BitswapMessage {
        wantlist: Some(Wantlist {
            entries: vec![WantEntry {
                block: cid.to_bytes(),
                priority: 1,
                cancel,
                want_type: WantType::Block as i32,
                send_dont_have: true,
                tokens: Vec::new(),
            }],
            full: false,
        }),
        blocks: Vec::new(),
        payload: Vec::new(),
        block_presences: Vec::new(),
        pending_bytes: 0,
        tokens: Vec::new(),
    }
}

async fn write_empty_bitswap_message<T>(io: &mut T) -> io::Result<()>
where
    T: AsyncWrite + Unpin,
{
    write_length_prefixed(io, &BitswapMessage::default().encode_to_vec()).await?;
    io.flush().await
}

async fn read_bitswap_blocks<T>(io: &mut T) -> io::Result<Vec<ReceivedBitswapBlock>>
where
    T: AsyncRead + Unpin,
{
    for _ in 0..4 {
        let bytes = read_length_prefixed(io, 2 * 1024 * 1024 + 4096).await?;
        let message = BitswapMessage::decode(bytes.as_slice()).map_err(invalid_data)?;
        let mut blocks = message
            .blocks
            .into_iter()
            .map(|data| ReceivedBitswapBlock { cid: None, data })
            .collect::<Vec<_>>();
        blocks.extend(message.payload.into_iter().map(|payload| {
            let cid = cid_from_bitswap_payload_prefix(&payload.prefix, &payload.data);
            ReceivedBitswapBlock {
                cid,
                data: payload.data,
            }
        }));
        if !blocks.is_empty() {
            return Ok(blocks);
        }
    }
    Ok(Vec::new())
}

fn collect_bitswap_result(
    requested: &Cid,
    blocks: Vec<ReceivedBitswapBlock>,
) -> Option<BitswapFetchResult> {
    let mut requested_block = None;
    let mut extra_blocks = Vec::new();

    for block in blocks {
        match block.cid {
            Some(block_cid) if &block_cid == requested => {
                if verify_block(requested, &block.data).is_ok() {
                    requested_block = Some(block.data);
                }
            }
            Some(block_cid) => {
                if verify_block(&block_cid, &block.data).is_ok() {
                    extra_blocks.push((block_cid, block.data));
                }
            }
            None => {
                if verify_block(requested, &block.data).is_ok() {
                    requested_block = Some(block.data);
                }
            }
        }
    }

    requested_block.map(|requested_block| BitswapFetchResult {
        requested_block,
        extra_blocks,
    })
}

fn cid_from_bitswap_payload_prefix(prefix: &[u8], data: &[u8]) -> Option<Cid> {
    let (version, rest) = unsigned_varint::decode::u64(prefix).ok()?;
    if !matches!(version, CID_VERSION_0 | CID_VERSION_1) {
        return None;
    }
    let (codec, rest) = unsigned_varint::decode::u64(rest).ok()?;
    let (hash_code, rest) = unsigned_varint::decode::u64(rest).ok()?;
    let (hash_len, rest) = unsigned_varint::decode::u64(rest).ok()?;
    if !rest.is_empty() {
        return None;
    }

    let hash_len = usize::try_from(hash_len).ok()?;
    let hash = match hash_code {
        HASH_SHA2_256 => {
            let hash = Code::Sha2_256.digest(data);
            if hash.digest().len() != hash_len {
                return None;
            }
            hash
        }
        HASH_IDENTITY => {
            if data.len() != hash_len {
                return None;
            }
            Multihash::<64>::wrap(HASH_IDENTITY, data).ok()?
        }
        _ => return None,
    };

    match version {
        CID_VERSION_0 if codec == CODEC_DAG_PB => Cid::new_v0(hash).ok(),
        CID_VERSION_1 => Some(Cid::new_v1(codec, hash)),
        _ => None,
    }
}

async fn read_length_prefixed<T>(io: &mut T, max_size: usize) -> io::Result<Vec<u8>>
where
    T: AsyncRead + Unpin,
{
    let len = read_varint_usize(io).await?;
    if len > max_size {
        return Err(invalid_data("bitswap message too large"));
    }
    let mut bytes = vec![0; len];
    io.read_exact(&mut bytes).await?;
    Ok(bytes)
}

async fn read_varint_usize<T>(io: &mut T) -> io::Result<usize>
where
    T: AsyncRead + Unpin,
{
    let mut value = 0usize;
    for shift in (0..35).step_by(7) {
        let mut byte = [0u8; 1];
        io.read_exact(&mut byte).await?;
        value |= usize::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(invalid_data("bitswap message length varint is too large"))
}

async fn write_length_prefixed<T>(io: &mut T, bytes: &[u8]) -> io::Result<()>
where
    T: AsyncWrite + Unpin,
{
    let len = u32::try_from(bytes.len()).map_err(|_| invalid_data("bitswap message too large"))?;
    let mut buffer = unsigned_varint::encode::u32_buffer();
    io.write_all(unsigned_varint::encode::u32(len, &mut buffer))
        .await?;
    io.write_all(bytes).await
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[derive(Clone, PartialEq, Message)]
struct BitswapMessage {
    #[prost(message, optional, tag = "1")]
    wantlist: Option<Wantlist>,
    #[prost(bytes = "vec", repeated, tag = "2")]
    blocks: Vec<Vec<u8>>,
    #[prost(message, repeated, tag = "3")]
    payload: Vec<BlockPayload>,
    #[prost(message, repeated, tag = "4")]
    block_presences: Vec<BlockPresence>,
    #[prost(int32, tag = "5")]
    pending_bytes: i32,
    #[prost(bytes = "vec", repeated, tag = "6")]
    tokens: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
struct Wantlist {
    #[prost(message, repeated, tag = "1")]
    entries: Vec<WantEntry>,
    #[prost(bool, tag = "2")]
    full: bool,
}

#[derive(Clone, PartialEq, Message)]
struct WantEntry {
    #[prost(bytes = "vec", tag = "1")]
    block: Vec<u8>,
    #[prost(int32, tag = "2")]
    priority: i32,
    #[prost(bool, tag = "3")]
    cancel: bool,
    #[prost(enumeration = "WantType", tag = "4")]
    want_type: i32,
    #[prost(bool, tag = "5")]
    send_dont_have: bool,
    #[prost(int32, repeated, tag = "7")]
    tokens: Vec<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
enum WantType {
    Block = 0,
    Have = 1,
}

#[derive(Clone, PartialEq, Message)]
struct BlockPayload {
    #[prost(bytes = "vec", tag = "1")]
    prefix: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    data: Vec<u8>,
    #[prost(int32, repeated, tag = "4")]
    tokens: Vec<i32>,
}

#[derive(Clone, PartialEq, Message)]
struct BlockPresence {
    #[prost(bytes = "vec", tag = "1")]
    cid: Vec<u8>,
    #[prost(int32, tag = "2")]
    type_pb: i32,
    #[prost(int32, repeated, tag = "4")]
    tokens: Vec<i32>,
}

#[cfg(test)]
mod bitswap_tests {
    use super::*;

    #[test]
    fn extracts_supported_peer_multiaddr() {
        let provider = parse_peer_id("12D3KooWNDpFqyse9kR7aZwgEzh4U1mL6Zz6jEuRNFXJxL5D2KPP");
        let (peer, addr) =
            parse_bitswap_multiaddr("/ip4/164.92.225.198/tcp/4001", provider).unwrap();
        assert_eq!(Some(peer), provider);
        assert_eq!(addr.to_string(), "/ip4/164.92.225.198/tcp/4001");
    }

    #[test]
    fn accepts_quic_multiaddr() {
        let provider = parse_peer_id("12D3KooWNDpFqyse9kR7aZwgEzh4U1mL6Zz6jEuRNFXJxL5D2KPP");
        let (peer, addr) =
            parse_bitswap_multiaddr("/ip4/164.92.225.198/udp/4001/quic-v1", provider).unwrap();
        assert_eq!(Some(peer), provider);
        assert_eq!(addr.to_string(), "/ip4/164.92.225.198/udp/4001/quic-v1");
    }

    #[test]
    fn rejects_relay_only_bitswap_multiaddr() {
        let provider = parse_peer_id("12D3KooWNDpFqyse9kR7aZwgEzh4U1mL6Zz6jEuRNFXJxL5D2KPP");
        assert!(
            parse_bitswap_multiaddr("/ip4/164.92.225.198/tcp/4001/p2p-circuit", provider,)
                .is_none()
        );
    }

    #[tokio::test]
    async fn deduplicates_and_caps_bitswap_peer_addresses() {
        let peer = "12D3KooWNDpFqyse9kR7aZwgEzh4U1mL6Zz6jEuRNFXJxL5D2KPP";
        let providers = vec![Provider::from_parts(
            Some(peer.to_string()),
            vec![
                "/dns4/example.com/tcp/4001".to_string(),
                "/ip4/164.92.225.198/udp/4001/quic-v1".to_string(),
                "/ip4/164.92.225.198/tcp/4001".to_string(),
                "/ip4/164.92.225.198/tcp/4001".to_string(),
                "/dns4/example.com/tcp/4002/ws".to_string(),
                "/ip4/164.92.225.199/tcp/4001".to_string(),
            ],
        )
        .unwrap()];

        let peers = bitswap_peers(&providers).await;

        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].addrs.len(), MAX_BITSWAP_ADDRS_PER_PEER);
        assert_eq!(
            peers[0].addrs[0].to_string(),
            "/ip4/164.92.225.198/tcp/4001"
        );
        assert_eq!(
            peers[0].addrs[1].to_string(),
            "/ip4/164.92.225.199/tcp/4001"
        );
    }

    #[test]
    fn decodes_bitswap_payload_prefix_to_cid() {
        let data = b"payload block";
        let cid = freedom_ipfs_core::cid_from_data(freedom_ipfs_core::CODEC_RAW, data);
        let prefix = bitswap_payload_prefix(&cid);

        assert_eq!(cid_from_bitswap_payload_prefix(&prefix, data), Some(cid));
    }

    #[test]
    fn decodes_cidv0_bitswap_payload_prefix_to_cid() {
        let data = b"legacy dag-pb payload";
        let cid = Cid::new_v0(Code::Sha2_256.digest(data)).unwrap();
        let mut prefix = Vec::new();
        append_uvarint(&mut prefix, CID_VERSION_0);
        append_uvarint(&mut prefix, freedom_ipfs_core::CODEC_DAG_PB);
        append_uvarint(&mut prefix, cid.hash().code());
        append_uvarint(&mut prefix, cid.hash().digest().len() as u64);

        assert_eq!(cid_from_bitswap_payload_prefix(&prefix, data), Some(cid));
    }

    #[test]
    fn collects_requested_and_extra_bitswap_payload_blocks() {
        let requested_data = b"requested block";
        let extra_data = b"extra linked block";
        let requested =
            freedom_ipfs_core::cid_from_data(freedom_ipfs_core::CODEC_RAW, requested_data);
        let extra = freedom_ipfs_core::cid_from_data(freedom_ipfs_core::CODEC_RAW, extra_data);

        let result = collect_bitswap_result(
            &requested,
            vec![
                ReceivedBitswapBlock {
                    cid: Some(extra),
                    data: extra_data.to_vec(),
                },
                ReceivedBitswapBlock {
                    cid: Some(requested),
                    data: requested_data.to_vec(),
                },
            ],
        )
        .unwrap();

        assert_eq!(result.requested_block, requested_data);
        assert_eq!(result.extra_blocks, vec![(extra, extra_data.to_vec())]);
    }

    #[test]
    fn cancel_bitswap_message_revokes_block_want() {
        let data = b"cancel me";
        let cid = freedom_ipfs_core::cid_from_data(freedom_ipfs_core::CODEC_RAW, data);
        let message = bitswap_want_message(&cid, true);
        let wantlist = message.wantlist.unwrap();
        let entry = wantlist.entries.first().unwrap();

        assert!(entry.cancel);
        assert_eq!(entry.block, cid.to_bytes());
        assert_eq!(entry.want_type, WantType::Block as i32);
    }

    fn bitswap_payload_prefix(cid: &Cid) -> Vec<u8> {
        let mut prefix = Vec::new();
        append_uvarint(&mut prefix, CID_VERSION_1);
        append_uvarint(&mut prefix, cid.codec());
        append_uvarint(&mut prefix, cid.hash().code());
        append_uvarint(&mut prefix, cid.hash().digest().len() as u64);
        prefix
    }

    fn append_uvarint(buffer: &mut Vec<u8>, value: u64) {
        let mut encode_buffer = unsigned_varint::encode::u64_buffer();
        buffer.extend_from_slice(unsigned_varint::encode::u64(value, &mut encode_buffer));
    }

    #[test]
    fn fetching_provider_records_cache_hits() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"cached retrieval";
        let cid = freedom_ipfs_core::cid_from_data(freedom_ipfs_core::CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();

        let provider = FetchingBlockProvider::new(
            store,
            freedom_ipfs_routing::DelegatedRoutingClient::new("http://127.0.0.1:9/routing/v1"),
        );

        let block = provider.get_block(&cid).unwrap().unwrap();
        assert_eq!(block.data(), data);
        assert_eq!(
            provider.stats(),
            RetrievalStats {
                cache_hits: 1,
                http_provider_blocks: 0,
                bitswap_blocks: 0,
            }
        );
    }
}
