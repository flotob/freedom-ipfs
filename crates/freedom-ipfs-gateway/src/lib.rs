use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use cid::Cid;
use freedom_ipfs_core::{parse_cid, BlockProvider};
use freedom_ipfs_namesys::{NameResolver, NamesysError};
use freedom_ipfs_store::SqliteBlockStore;
use freedom_ipfs_unixfs::{file_size, read_file_range, UnixfsError};
use futures::stream;
use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

pub const DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS: usize = 8;
const GATEWAY_STREAM_CHUNK_SIZE: u64 = 64 * 1024;

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    max_concurrent_requests: usize,
}

impl GatewayConfig {
    pub fn new(max_concurrent_requests: usize) -> Self {
        Self {
            max_concurrent_requests: max_concurrent_requests.max(1),
        }
    }

    pub fn max_concurrent_requests(&self) -> usize {
        self.max_concurrent_requests
    }
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self::new(DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS)
    }
}

#[derive(Clone)]
pub struct GatewayState {
    provider: Arc<dyn BlockProvider>,
    name_resolver: Arc<dyn NameResolver>,
    request_limiter: Arc<Semaphore>,
}

impl GatewayState {
    pub fn new(store: SqliteBlockStore) -> Self {
        Self::with_provider(Arc::new(store))
    }

    pub fn with_provider(provider: Arc<dyn BlockProvider>) -> Self {
        Self::with_provider_config(provider, GatewayConfig::default())
    }

    pub fn with_provider_config(provider: Arc<dyn BlockProvider>, config: GatewayConfig) -> Self {
        Self::with_provider_and_name_resolver_config(
            provider,
            Arc::new(OfflineNameResolver),
            config,
        )
    }

    pub fn with_provider_and_name_resolver(
        provider: Arc<dyn BlockProvider>,
        name_resolver: Arc<dyn NameResolver>,
    ) -> Self {
        Self::with_provider_and_name_resolver_config(
            provider,
            name_resolver,
            GatewayConfig::default(),
        )
    }

    pub fn with_provider_and_name_resolver_config(
        provider: Arc<dyn BlockProvider>,
        name_resolver: Arc<dyn NameResolver>,
        config: GatewayConfig,
    ) -> Self {
        Self {
            provider,
            name_resolver,
            request_limiter: Arc::new(Semaphore::new(config.max_concurrent_requests())),
        }
    }
}

pub fn router(store: SqliteBlockStore) -> Router {
    router_with_provider(Arc::new(store))
}

pub fn router_with_provider(provider: Arc<dyn BlockProvider>) -> Router {
    router_with_provider_and_name_resolver(provider, Arc::new(OfflineNameResolver))
}

pub fn router_with_provider_config(
    provider: Arc<dyn BlockProvider>,
    config: GatewayConfig,
) -> Router {
    router_with_provider_and_name_resolver_config(provider, Arc::new(OfflineNameResolver), config)
}

pub fn router_with_provider_and_name_resolver(
    provider: Arc<dyn BlockProvider>,
    name_resolver: Arc<dyn NameResolver>,
) -> Router {
    router_with_provider_and_name_resolver_config(provider, name_resolver, GatewayConfig::default())
}

pub fn router_with_provider_and_name_resolver_config(
    provider: Arc<dyn BlockProvider>,
    name_resolver: Arc<dyn NameResolver>,
    config: GatewayConfig,
) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ipfs/{*path}", get(ipfs_get))
        .route("/ipns/{*path}", get(ipns_get))
        .with_state(GatewayState::with_provider_and_name_resolver_config(
            provider,
            name_resolver,
            config,
        ))
}

#[derive(Debug, Clone)]
struct OfflineNameResolver;

#[async_trait::async_trait]
impl NameResolver for OfflineNameResolver {
    async fn resolve_name(&self, name: &str) -> freedom_ipfs_namesys::Result<String> {
        Err(NamesysError::NotFound(name.to_string()))
    }
}

pub async fn serve(store: SqliteBlockStore, addr: SocketAddr) -> std::io::Result<SocketAddr> {
    serve_config(store, addr, GatewayConfig::default()).await
}

pub async fn serve_config(
    store: SqliteBlockStore,
    addr: SocketAddr,
    config: GatewayConfig,
) -> std::io::Result<SocketAddr> {
    serve_with_provider_config(Arc::new(store), addr, config).await
}

pub async fn serve_with_provider(
    provider: Arc<dyn BlockProvider>,
    addr: SocketAddr,
) -> std::io::Result<SocketAddr> {
    serve_with_provider_config(provider, addr, GatewayConfig::default()).await
}

pub async fn serve_with_provider_config(
    provider: Arc<dyn BlockProvider>,
    addr: SocketAddr,
    config: GatewayConfig,
) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    axum::serve(listener, router_with_provider_config(provider, config)).await?;
    Ok(bound)
}

pub async fn serve_with_provider_and_name_resolver_config(
    provider: Arc<dyn BlockProvider>,
    name_resolver: Arc<dyn NameResolver>,
    addr: SocketAddr,
    config: GatewayConfig,
) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    axum::serve(
        listener,
        router_with_provider_and_name_resolver_config(provider, name_resolver, config),
    )
    .await?;
    Ok(bound)
}

async fn health() -> &'static str {
    "ok\n"
}

async fn ipfs_get(
    State(state): State<GatewayState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(_permit) = state.request_limiter.clone().try_acquire_owned() else {
        return gateway_error(GatewayError::Busy);
    };

    match serve_ipfs_path(state.provider.clone(), &path, headers.get(RANGE)).await {
        Ok(response) => response,
        Err(err) => gateway_error(err),
    }
}

async fn ipns_get(
    State(state): State<GatewayState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(_permit) = state.request_limiter.clone().try_acquire_owned() else {
        return gateway_error(GatewayError::Busy);
    };

    match serve_ipns_path(
        state.provider.clone(),
        state.name_resolver.as_ref(),
        &path,
        headers.get(RANGE),
    )
    .await
    {
        Ok(response) => response,
        Err(err) => gateway_error(err),
    }
}

async fn serve_ipfs_path(
    provider: Arc<dyn BlockProvider>,
    path: &str,
    range: Option<&HeaderValue>,
) -> Result<Response, GatewayError> {
    let (cid, unixfs_path) = split_ipfs_path(path)?;
    let (served_path, len) = served_file_path(provider.as_ref(), &cid, unixfs_path)?;
    let mime = mime_guess::from_path(&served_path)
        .first_or_octet_stream()
        .to_string();

    let response = if let Some(range) = range {
        ranged_response(provider.as_ref(), &cid, &served_path, range, &mime)?
    } else {
        streaming_response(provider, cid, served_path, len, &mime)?
    };
    Ok(response)
}

fn served_file_path(
    provider: &dyn BlockProvider,
    cid: &Cid,
    unixfs_path: &str,
) -> Result<(String, u64), GatewayError> {
    match file_size(provider, cid, unixfs_path) {
        Ok(len) => Ok((unixfs_path.to_string(), len)),
        Err(UnixfsError::IsDirectory) => {
            let index_path = append_path(unixfs_path, "index.html");
            let len = file_size(provider, cid, &index_path).map_err(GatewayError::Unixfs)?;
            Ok((index_path, len))
        }
        Err(err) => Err(GatewayError::Unixfs(err)),
    }
}

fn split_ipfs_path(path: &str) -> Result<(Cid, &str), GatewayError> {
    let mut parts = path.splitn(2, '/');
    let cid = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| {
            GatewayError::BadRequest("expected /ipfs/{cid} or /ipfs/{cid}/{path}".into())
        })?;
    let cid = parse_cid(cid).map_err(|err| GatewayError::BadRequest(err.to_string()))?;
    let unixfs_path = parts.next().unwrap_or_default();
    validate_relative_gateway_path(unixfs_path)?;
    Ok((cid, unixfs_path))
}

async fn serve_ipns_path(
    provider: Arc<dyn BlockProvider>,
    name_resolver: &dyn NameResolver,
    path: &str,
    range: Option<&HeaderValue>,
) -> Result<Response, GatewayError> {
    let mut target = format!("/ipns/{path}");

    for _ in 0..4 {
        if let Some(ipfs) = target.strip_prefix("/ipfs/") {
            return serve_ipfs_path(provider.clone(), ipfs, range).await;
        }

        let Some(ipns) = target.strip_prefix("/ipns/") else {
            return Err(GatewayError::BadGateway(format!(
                "dnslink target is not /ipfs or /ipns: {target}"
            )));
        };
        let (name, rest) = split_name_path(ipns)?;
        let resolved = name_resolver.resolve_name(name).await.map_err(|err| {
            if matches!(err, NamesysError::NotFound(_)) {
                GatewayError::NotFound(format!("name not found: {name}"))
            } else {
                GatewayError::BadGateway(format!("ipns/dnslink resolution failed: {err}"))
            }
        })?;
        target = append_path(&resolved, rest);
    }

    Err(GatewayError::BadRequest(
        "dnslink recursion limit exceeded".into(),
    ))
}

fn split_name_path(path: &str) -> Result<(&str, &str), GatewayError> {
    let mut parts = path.splitn(2, '/');
    let name = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| {
            GatewayError::BadRequest("expected /ipns/{name} or /ipns/{name}/{path}".into())
        })?;
    reject_traversal_segment(name)?;
    let rest = parts.next().unwrap_or_default();
    validate_relative_gateway_path(rest)?;
    Ok((name, rest))
}

fn validate_relative_gateway_path(path: &str) -> Result<(), GatewayError> {
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        reject_traversal_segment(segment)?;
    }
    Ok(())
}

fn reject_traversal_segment(segment: &str) -> Result<(), GatewayError> {
    if is_traversal_segment(segment) {
        return Err(GatewayError::BadRequest(
            "path traversal segments are not allowed".into(),
        ));
    }
    Ok(())
}

fn is_traversal_segment(segment: &str) -> bool {
    matches!(segment, "." | "..")
        || percent_decode_ascii(segment)
            .as_deref()
            .is_some_and(|decoded| matches!(decoded, "." | ".."))
}

fn percent_decode_ascii(segment: &str) -> Option<String> {
    if !segment.as_bytes().contains(&b'%') {
        return None;
    }
    let mut decoded = Vec::with_capacity(segment.len());
    let bytes = segment.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_value(high)? << 4 | hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn append_path(base: &str, rest: &str) -> String {
    if rest.is_empty() {
        base.to_string()
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            rest.trim_start_matches('/')
        )
    }
}

fn streaming_response(
    provider: Arc<dyn BlockProvider>,
    cid: Cid,
    path: String,
    len: u64,
    mime: &str,
) -> Result<Response, GatewayError> {
    let provider = Arc::new(ScopedBlockProvider::new(provider));
    let stream = stream::unfold(0u64, move |offset| {
        let provider = provider.clone();
        let path = path.clone();
        async move {
            if offset >= len {
                return None;
            }
            let end = (offset + GATEWAY_STREAM_CHUNK_SIZE - 1).min(len - 1);
            let next = end + 1;
            let chunk = read_file_range(
                provider.as_ref() as &dyn BlockProvider,
                &cid,
                &path,
                offset,
                end,
            )
            .map(Bytes::from)
            .map_err(|err| io::Error::other(err.to_string()));
            Some((chunk, next))
        }
    });

    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(mime).map_err(|err| GatewayError::Internal(err.to_string()))?,
    );
    response
        .headers_mut()
        .insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&len.to_string())
            .map_err(|err| GatewayError::Internal(err.to_string()))?,
    );
    Ok(response)
}

struct ScopedBlockProvider {
    inner: Arc<dyn BlockProvider>,
    retained: Mutex<HashSet<Cid>>,
}

impl ScopedBlockProvider {
    fn new(inner: Arc<dyn BlockProvider>) -> Self {
        Self {
            inner,
            retained: Mutex::new(HashSet::new()),
        }
    }
}

impl BlockProvider for ScopedBlockProvider {
    fn get_block(&self, cid: &Cid) -> freedom_ipfs_core::Result<Option<freedom_ipfs_core::Block>> {
        let mut retained_here = false;
        {
            let mut retained = self.retained.lock().map_err(|err| {
                freedom_ipfs_core::CoreError::Storage(format!(
                    "stream retention lock poisoned: {err}"
                ))
            })?;
            if retained.insert(*cid) {
                self.inner.retain_block(cid)?;
                retained_here = true;
            }
        }

        match self.inner.get_block(cid)? {
            Some(block) => Ok(Some(block)),
            None => {
                if retained_here {
                    if let Ok(mut retained) = self.retained.lock() {
                        retained.remove(cid);
                    }
                    self.inner.release_block(cid);
                }
                Ok(None)
            }
        }
    }
}

impl Drop for ScopedBlockProvider {
    fn drop(&mut self) {
        if let Ok(mut retained) = self.retained.lock() {
            for cid in retained.drain() {
                self.inner.release_block(&cid);
            }
        }
    }
}

fn ranged_response(
    provider: &dyn BlockProvider,
    cid: &Cid,
    path: &str,
    range: &HeaderValue,
    mime: &str,
) -> Result<Response, GatewayError> {
    let range = range
        .to_str()
        .map_err(|_| GatewayError::BadRequest("invalid Range header".into()))?;
    let Some(spec) = range.strip_prefix("bytes=") else {
        return Err(GatewayError::BadRequest(
            "only bytes ranges are supported".into(),
        ));
    };
    let total_len = file_size(provider, cid, path).map_err(GatewayError::Unixfs)?;
    let (start, end) = parse_range_spec(spec, total_len)?;
    let slice = read_file_range(provider, cid, path, start, end).map_err(GatewayError::Unixfs)?;
    let mut response =
        (StatusCode::PARTIAL_CONTENT, Body::from(Bytes::from(slice))).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(mime).map_err(|err| GatewayError::Internal(err.to_string()))?,
    );
    response
        .headers_mut()
        .insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response.headers_mut().insert(
        CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {start}-{end}/{total_len}"))
            .map_err(|err| GatewayError::Internal(err.to_string()))?,
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&(end - start + 1).to_string())
            .map_err(|err| GatewayError::Internal(err.to_string()))?,
    );
    Ok(response)
}

fn parse_range_spec(spec: &str, len: u64) -> Result<(u64, u64), GatewayError> {
    if len == 0 {
        return Err(GatewayError::RangeNotSatisfiable);
    }
    let (start, end) = spec
        .split_once('-')
        .ok_or_else(|| GatewayError::BadRequest("invalid range syntax".into()))?;

    if start.is_empty() {
        let suffix = end
            .parse::<u64>()
            .map_err(|_| GatewayError::BadRequest("invalid suffix range".into()))?;
        if suffix == 0 {
            return Err(GatewayError::RangeNotSatisfiable);
        }
        let start = len.saturating_sub(suffix);
        return Ok((start, len - 1));
    }

    let start = start
        .parse::<u64>()
        .map_err(|_| GatewayError::BadRequest("invalid range start".into()))?;
    let end = if end.is_empty() {
        len - 1
    } else {
        end.parse::<u64>()
            .map_err(|_| GatewayError::BadRequest("invalid range end".into()))?
    };

    if start >= len || start > end {
        return Err(GatewayError::RangeNotSatisfiable);
    }
    Ok((start, end.min(len - 1)))
}

#[derive(Debug)]
enum GatewayError {
    BadRequest(String),
    NotFound(String),
    Unixfs(UnixfsError),
    RangeNotSatisfiable,
    Busy,
    BadGateway(String),
    Internal(String),
}

fn gateway_error(err: GatewayError) -> Response {
    let (status, title, detail) = match err {
        GatewayError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "Bad Request", msg),
        GatewayError::NotFound(msg) => (StatusCode::NOT_FOUND, "Not Found", msg),
        GatewayError::Unixfs(UnixfsError::NotFound(_))
        | GatewayError::Unixfs(UnixfsError::PathNotFound(_)) => {
            (StatusCode::NOT_FOUND, "Not Found", "not found".into())
        }
        GatewayError::Unixfs(UnixfsError::IsDirectory) => (
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "directory listing is not implemented yet".into(),
        ),
        GatewayError::Unixfs(err) if is_timeout_error(&err) => (
            StatusCode::GATEWAY_TIMEOUT,
            "Gateway Timeout",
            format!("retrieval timeout: {err}"),
        ),
        GatewayError::Unixfs(err) => (
            StatusCode::BAD_GATEWAY,
            "Bad Gateway",
            format!("unixfs error: {err}"),
        ),
        GatewayError::RangeNotSatisfiable => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            "Range Not Satisfiable",
            "range not satisfiable".into(),
        ),
        GatewayError::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "gateway busy".into(),
        ),
        GatewayError::BadGateway(msg) => (StatusCode::BAD_GATEWAY, "Bad Gateway", msg),
        GatewayError::Internal(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            msg,
        ),
    };

    html_error_response(status, title, &detail)
}

fn html_error_response(status: StatusCode, title: &str, detail: &str) -> Response {
    let body = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{code} {title}</title><meta name="viewport" content="width=device-width,initial-scale=1"></head><body><main><h1>{title}</h1><p>{detail}</p></main></body></html>"#,
        code = status.as_u16(),
        title = escape_html(title),
        detail = escape_html(detail)
    );
    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn is_timeout_error(err: &UnixfsError) -> bool {
    let UnixfsError::Provider(message) = err else {
        return false;
    };
    let message = message.to_ascii_lowercase();
    message.contains("timed out") || message.contains("timeout")
}

#[cfg(test)]
mod tests {
    use super::*;
    use freedom_ipfs_core::{
        cid_from_data, Block, CoreError, Result as CoreResult, CODEC_DAG_PB, CODEC_RAW,
    };
    use freedom_ipfs_namesys::{NamesysError, Result as NamesysResult};
    use prost::Message;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn serves_cached_raw_block_through_gateway() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"<html>offline</html>";
        let cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(store);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipfs/{cid}");
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap(), Bytes::from_static(data));
    }

    #[tokio::test]
    async fn serves_directory_index_with_path_based_mime_type() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let index = b"<main>browser data plane</main>";
        let index_block = test_pb_file(index);
        let index_cid = cid_from_data(CODEC_DAG_PB, &index_block);
        store.put_block(&index_cid, &index_block).unwrap();

        let dir_block = test_pb_directory(vec![test_link("index.html", &index_cid)]);
        let dir_cid = cid_from_data(CODEC_DAG_PB, &dir_block);
        store.put_block(&dir_cid, &dir_block).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(store);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipfs/{dir_cid}");
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            HeaderValue::from_static("text/html")
        );
        assert_eq!(response.bytes().await.unwrap(), Bytes::from_static(index));
    }

    #[tokio::test]
    async fn supports_byte_ranges() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"0123456789";
        let cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(store);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipfs/{cid}");
        let client = reqwest::Client::new();
        let response = client
            .get(url)
            .header(RANGE, "bytes=2-5")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.bytes().await.unwrap(), Bytes::from_static(b"2345"));
    }

    #[tokio::test]
    async fn rejects_invalid_and_unsatisfiable_byte_ranges() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"0123456789";
        let cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(store);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipfs/{cid}");
        let client = reqwest::Client::new();
        for (range, status) in [
            ("items=2-5", StatusCode::BAD_REQUEST),
            ("bytes=abc-5", StatusCode::BAD_REQUEST),
            ("bytes=8-2", StatusCode::RANGE_NOT_SATISFIABLE),
            ("bytes=20-30", StatusCode::RANGE_NOT_SATISFIABLE),
            ("bytes=-0", StatusCode::RANGE_NOT_SATISFIABLE),
        ] {
            let response = client.get(&url).header(RANGE, range).send().await.unwrap();
            assert_eq!(response.status(), status, "{range}");
        }
    }

    #[tokio::test]
    async fn returns_browser_facing_html_error_pages() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"do not traverse";
        let cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(store);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipfs/{cid}/%2e%2e/index.html");
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            HeaderValue::from_static("text/html; charset=utf-8")
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("<!doctype html>"));
        assert!(body.contains("<h1>Bad Request</h1>"));
        assert!(body.contains("<p>"));
    }

    #[tokio::test]
    async fn html_error_pages_escape_details() {
        let response = gateway_error(GatewayError::BadRequest(
            r#"<script>alert("cid")</script> & bad"#.into(),
        ));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("&lt;script&gt;alert(&quot;cid&quot;)&lt;/script&gt; &amp; bad"));
        assert!(!body.contains("<script>"));
    }

    #[tokio::test]
    async fn streams_full_response_across_chunks() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = (0..(GATEWAY_STREAM_CHUNK_SIZE as usize + 17))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let cid = cid_from_data(CODEC_RAW, &data);
        store.put_block(&cid, &data).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(store);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipfs/{cid}");
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            data.len().to_string()
        );
        assert_eq!(response.bytes().await.unwrap(), Bytes::from(data));
    }

    #[tokio::test]
    async fn maps_provider_timeouts_to_gateway_timeout() {
        let data = b"timeout target";
        let cid = cid_from_data(CODEC_RAW, data);
        let provider = Arc::new(TimeoutProvider);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router_with_provider(provider);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipfs/{cid}");
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn default_router_does_not_resolve_names_online() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(store);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipns/example.com");
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn does_not_expose_kubo_rpc_or_webui_routes() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(store);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        for path in ["/api/v0/version", "/api/v0/id", "/api/v0/refs", "/webui"] {
            let get = client
                .get(format!("http://{addr}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(get.status(), StatusCode::NOT_FOUND, "GET {path}");
            assert_ne!(
                get.headers().get(CONTENT_TYPE),
                Some(&HeaderValue::from_static("application/json")),
                "GET {path}"
            );

            let post = client
                .post(format!("http://{addr}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(post.status(), StatusCode::NOT_FOUND, "POST {path}");
            assert_ne!(
                post.headers().get(CONTENT_TYPE),
                Some(&HeaderValue::from_static("application/json")),
                "POST {path}"
            );
        }
    }

    #[tokio::test]
    async fn resolves_ipns_path_through_name_resolver() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"<html>ipns</html>";
        let cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router_with_provider_and_name_resolver(
            Arc::new(store),
            Arc::new(StaticNameResolver {
                name: "k51fixture".to_string(),
                target: format!("/ipfs/{cid}"),
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipns/k51fixture");
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap(), Bytes::from_static(data));
    }

    #[tokio::test]
    async fn rejects_ipns_resolution_loops() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router_with_provider_and_name_resolver(
            Arc::new(store),
            Arc::new(StaticNameResolver {
                name: "loop.example".to_string(),
                target: "/ipns/loop.example".to_string(),
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/ipns/loop.example");
        let response = reqwest::get(url).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("recursion limit exceeded"));
    }

    #[tokio::test]
    async fn rejects_path_traversal_segments() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"do not traverse";
        let cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router_with_provider_and_name_resolver(
            Arc::new(store),
            Arc::new(StaticNameResolver {
                name: "example.com".to_string(),
                target: format!("/ipfs/{cid}"),
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        assert!(split_ipfs_path(&format!("{cid}/../index.html")).is_err());
        assert!(split_ipfs_path(&format!("{cid}/./index.html")).is_err());
        assert!(split_ipfs_path(&format!("{cid}/%2e%2e/index.html")).is_err());
        assert!(split_ipfs_path(&format!("{cid}/%2e/index.html")).is_err());
        assert!(split_name_path("../index.html").is_err());
        assert!(split_name_path("example.com/%2e%2e/index.html").is_err());

        for path in [format!("/ipfs/{cid}/%2e%2e/index.html")] {
            let url = format!("http://{addr}{path}");
            let response = reqwest::get(url).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_requests_above_concurrency_limit() {
        let data = b"limited gateway";
        let cid = cid_from_data(CODEC_RAW, data);
        let entered = Arc::new(AtomicBool::new(false));
        let provider = Arc::new(SlowProvider {
            cid,
            data: data.to_vec(),
            entered: entered.clone(),
        });

        let state = GatewayState::with_provider_config(provider, GatewayConfig::new(1));
        let first = tokio::spawn(ipfs_get(
            State(state.clone()),
            Path(cid.to_string()),
            HeaderMap::new(),
        ));

        for _ in 0..50 {
            if entered.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(entered.load(Ordering::SeqCst));

        let second = ipfs_get(State(state), Path(cid.to_string()), HeaderMap::new()).await;
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);

        let first = first.await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
    }

    struct StaticNameResolver {
        name: String,
        target: String,
    }

    #[async_trait::async_trait]
    impl NameResolver for StaticNameResolver {
        async fn resolve_name(&self, name: &str) -> NamesysResult<String> {
            if name == self.name {
                Ok(self.target.clone())
            } else {
                Err(NamesysError::NotFound(name.to_string()))
            }
        }
    }

    struct SlowProvider {
        cid: Cid,
        data: Vec<u8>,
        entered: Arc<AtomicBool>,
    }

    impl BlockProvider for SlowProvider {
        fn get_block(&self, cid: &Cid) -> CoreResult<Option<Block>> {
            if cid != &self.cid {
                return Err(CoreError::Storage("unexpected cid".into()));
            }
            self.entered.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            Ok(Some(Block::unchecked(*cid, self.data.clone())))
        }
    }

    struct TimeoutProvider;

    impl BlockProvider for TimeoutProvider {
        fn get_block(&self, _cid: &Cid) -> CoreResult<Option<Block>> {
            Err(CoreError::Storage("bitswap request timed out".into()))
        }
    }

    #[derive(Clone, PartialEq, Message)]
    struct TestPbNode {
        #[prost(bytes = "vec", optional, tag = "1")]
        data: Option<Vec<u8>>,
        #[prost(message, repeated, tag = "2")]
        links: Vec<TestPbLink>,
    }

    #[derive(Clone, PartialEq, Message)]
    struct TestPbLink {
        #[prost(bytes = "vec", optional, tag = "1")]
        hash: Option<Vec<u8>>,
        #[prost(string, optional, tag = "2")]
        name: Option<String>,
        #[prost(uint64, optional, tag = "3")]
        tsize: Option<u64>,
    }

    #[derive(Clone, PartialEq, Message)]
    struct TestUnixfsData {
        #[prost(enumeration = "TestDataType", optional, tag = "1")]
        r#type: Option<i32>,
        #[prost(bytes = "vec", optional, tag = "2")]
        data: Option<Vec<u8>>,
        #[prost(uint64, optional, tag = "3")]
        filesize: Option<u64>,
        #[prost(uint64, repeated, tag = "4")]
        blocksizes: Vec<u64>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
    #[repr(i32)]
    enum TestDataType {
        Directory = 1,
        File = 2,
    }

    fn test_pb_file(data: &[u8]) -> Vec<u8> {
        TestPbNode {
            data: Some(
                TestUnixfsData {
                    r#type: Some(TestDataType::File as i32),
                    data: Some(data.to_vec()),
                    filesize: Some(data.len() as u64),
                    blocksizes: Vec::new(),
                }
                .encode_to_vec(),
            ),
            links: Vec::new(),
        }
        .encode_to_vec()
    }

    fn test_pb_directory(links: Vec<TestPbLink>) -> Vec<u8> {
        TestPbNode {
            data: Some(
                TestUnixfsData {
                    r#type: Some(TestDataType::Directory as i32),
                    data: Some(Vec::new()),
                    filesize: Some(0),
                    blocksizes: Vec::new(),
                }
                .encode_to_vec(),
            ),
            links,
        }
        .encode_to_vec()
    }

    fn test_link(name: &str, cid: &Cid) -> TestPbLink {
        TestPbLink {
            hash: Some(cid.to_bytes()),
            name: Some(name.to_string()),
            tsize: None,
        }
    }
}
