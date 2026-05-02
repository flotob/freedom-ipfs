use cid::Cid;
use freedom_ipfs_core::{verify_block, Block, BlockProvider, CoreError, Result as CoreResult};
use freedom_ipfs_namesys::{CloudflareDohResolver, DnsTxtResolver};
use freedom_ipfs_routing::{DelegatedRoutingClient, Provider};
use freedom_ipfs_store::SqliteBlockStore;
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use futures::stream::{select_all, FuturesUnordered};
use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::StreamProtocol;
use libp2p::{noise, tcp, tls, yamux, Multiaddr, PeerId, SwarmBuilder};
use libp2p_stream::{Control as StreamControl, IncomingStreams};
use prost::Message;
use std::io;
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;
use tokio::time::timeout;
use url::Url;

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

#[derive(Clone)]
pub struct HttpRetriever {
    client: reqwest::Client,
    routing: DelegatedRoutingClient,
    store: SqliteBlockStore,
}

impl HttpRetriever {
    pub fn new(routing: DelegatedRoutingClient, store: SqliteBlockStore) -> Self {
        Self {
            client: reqwest::Client::new(),
            routing,
            store,
        }
    }

    pub async fn fetch_block(&self, cid: &Cid) -> Result<Block> {
        if let Some(block) = self.store.get(cid)? {
            return Ok(block);
        }

        let providers = self.routing.providers(cid).await?;
        self.fetch_from_providers(cid, &providers).await
    }

    pub async fn fetch_from_providers(&self, cid: &Cid, providers: &[Provider]) -> Result<Block> {
        for provider in providers {
            for base in &provider.http_urls {
                match self.fetch_from_http_provider(cid, base).await {
                    Ok(block) => return Ok(block),
                    Err(_) => continue,
                }
            }
        }
        match self.fetch_from_bitswap_providers(cid, providers).await {
            Ok(block) => Ok(block),
            Err(RetrievalError::NoBitswapProviders) => Err(RetrievalError::NoHttpProviders),
            Err(err) => Err(err),
        }
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
        let peers = bitswap_peers(providers).await;
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

        let result = timeout(Duration::from_secs(45), async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            fetch_bitswap_over_streams(control, incoming, peer_ids, *cid).await
        })
        .await
        .map_err(|_| RetrievalError::BitswapTimeout)?;
        swarm_task.abort();
        let data = result?;
        self.store.put_block(cid, &data)?;
        Ok(Block::unchecked(*cid, data))
    }
}

#[derive(Clone)]
pub struct FetchingBlockProvider {
    store: SqliteBlockStore,
    retriever: HttpRetriever,
}

impl FetchingBlockProvider {
    pub fn new(store: SqliteBlockStore, routing: DelegatedRoutingClient) -> Self {
        let retriever = HttpRetriever::new(routing, store.clone());
        Self { store, retriever }
    }
}

impl BlockProvider for FetchingBlockProvider {
    fn get_block(&self, cid: &Cid) -> CoreResult<Option<Block>> {
        if let Some(block) = self
            .store
            .get(cid)
            .map_err(|err| CoreError::Storage(err.to_string()))?
        {
            return Ok(Some(block));
        }

        let fetched = match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                tokio::task::block_in_place(|| handle.block_on(self.retriever.fetch_block(cid)))
            }
            Err(_) => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| CoreError::Storage(err.to_string()))?;
                runtime.block_on(self.retriever.fetch_block(cid))
            }
        };

        match fetched {
            Ok(block) => Ok(Some(block)),
            Err(RetrievalError::NoHttpProviders | RetrievalError::NoBitswapProviders) => Ok(None),
            Err(err) => Err(CoreError::Storage(err.to_string())),
        }
    }
}

#[derive(Clone)]
struct BitswapPeer {
    id: PeerId,
    addrs: Vec<Multiaddr>,
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
                peers.push(BitswapPeer { id, addrs });
            }
        }
    }

    peers
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
    for protocol in addr.iter() {
        match protocol {
            Protocol::Tcp(_) => has_tcp = true,
            Protocol::Udp(_)
            | Protocol::Quic
            | Protocol::QuicV1
            | Protocol::WebTransport
            | Protocol::WebRTC
            | Protocol::WebRTCDirect
            | Protocol::P2pWebRtcDirect
            | Protocol::Certhash(_) => return false,
            _ => {}
        }
    }
    has_tcp
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
) -> Result<Vec<u8>> {
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
                        let _ = write_empty_bitswap_message(&mut stream).await;
                        for data in blocks {
                            if verify_block(&cid, &data).is_ok() {
                                return Ok(data);
                            }
                        }
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
) -> std::result::Result<Vec<u8>, String> {
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

        write_bitswap_want(&mut stream, &cid)
            .await
            .map_err(|err| format!("{peer_id} {protocol_name}: write failed: {err}"))?;
        let blocks = timeout(Duration::from_secs(10), read_bitswap_blocks(&mut stream))
            .await
            .map_err(|_| format!("{peer_id} {protocol_name}: read timed out"))?
            .map_err(|err| format!("{peer_id} {protocol_name}: read failed: {err}"))?;
        for data in blocks {
            if verify_block(&cid, &data).is_ok() {
                return Ok(data);
            }
        }
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
    let message = BitswapMessage {
        wantlist: Some(Wantlist {
            entries: vec![WantEntry {
                block: cid.to_bytes(),
                priority: 1,
                cancel: false,
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
    };
    write_length_prefixed(io, &message.encode_to_vec()).await?;
    io.flush().await
}

async fn write_empty_bitswap_message<T>(io: &mut T) -> io::Result<()>
where
    T: AsyncWrite + Unpin,
{
    write_length_prefixed(io, &BitswapMessage::default().encode_to_vec()).await?;
    io.flush().await
}

async fn read_bitswap_blocks<T>(io: &mut T) -> io::Result<Vec<Vec<u8>>>
where
    T: AsyncRead + Unpin,
{
    for _ in 0..4 {
        let bytes = read_length_prefixed(io, 2 * 1024 * 1024 + 4096).await?;
        let message = BitswapMessage::decode(bytes.as_slice()).map_err(invalid_data)?;
        let mut blocks = message.blocks;
        blocks.extend(message.payload.into_iter().map(|payload| payload.data));
        if !blocks.is_empty() {
            return Ok(blocks);
        }
    }
    Ok(Vec::new())
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
    fn rejects_quic_multiaddr_for_tcp_client() {
        let provider = parse_peer_id("12D3KooWNDpFqyse9kR7aZwgEzh4U1mL6Zz6jEuRNFXJxL5D2KPP");
        assert!(
            parse_bitswap_multiaddr("/ip4/164.92.225.198/udp/4001/quic-v1", provider,).is_none()
        );
    }
}
