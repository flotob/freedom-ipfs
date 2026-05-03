use axum::http::StatusCode;
use freedom_ipfs_gateway::router_with_provider_and_name_resolver;
use freedom_ipfs_namesys::{
    CachedNameResolver, CloudflareDohResolver, DefaultNameResolver, DelegatedIpnsResolver,
    FallbackIpnsResolver,
};
use freedom_ipfs_retrieval::FetchingBlockProvider;
use freedom_ipfs_routing::{
    AutoRoutingClient, DelegatedRoutingClient, DhtIpnsResolver, LightDhtClient,
    DEFAULT_DELEGATED_ROUTER,
};
use freedom_ipfs_store::SqliteBlockStore;
use serde::Deserialize;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

const REQUEST_ATTEMPTS: usize = 3;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "network smoke test; set FREEDOM_IPFS_LIVE_PATHS=/ipfs/<cid>,/ipns/<name>"]
async fn live_gateway_fetches_real_paths_without_public_gateway_fallback() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let mut paths = env::var("FREEDOM_IPFS_LIVE_PATHS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let ens_names = env::var("FREEDOM_IPFS_LIVE_ENS")
        .unwrap_or_else(|_| "vitalik.eth,daicowtf.eth".to_string());
    for name in ens_names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let resolved = resolve_ens_contenthash(name).await;
        eprintln!("{name} resolved to {resolved}");
        paths.push(resolved);
    }
    assert!(
        !paths.is_empty(),
        "set FREEDOM_IPFS_LIVE_PATHS or FREEDOM_IPFS_LIVE_ENS"
    );

    let router = env::var("FREEDOM_IPFS_DELEGATED_ROUTER")
        .unwrap_or_else(|_| DEFAULT_DELEGATED_ROUTER.to_string());
    let store = SqliteBlockStore::in_memory(256 * 1024 * 1024).unwrap();
    let dht = LightDhtClient::default();
    let routing = AutoRoutingClient::new(DelegatedRoutingClient::new(router.clone()), dht.clone());
    let provider = Arc::new(FetchingBlockProvider::new(store, routing));
    let stats_provider = provider.clone();
    let name_resolver = Arc::new(CachedNameResolver::new(DefaultNameResolver::new(
        CloudflareDohResolver::default(),
        FallbackIpnsResolver::new(
            DelegatedIpnsResolver::new(router),
            DhtIpnsResolver::new(dht),
        ),
    )));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router_with_provider_and_name_resolver(provider, name_resolver),
        )
        .await
        .unwrap();
    });
    eprintln!("local gateway listening on http://{addr} with auto routing");

    let client = reqwest::Client::new();
    for path in paths {
        assert!(
            path.starts_with("/ipfs/") || path.starts_with("/ipns/"),
            "live path must be externally resolved into /ipfs or /ipns form: {path}"
        );
        let url = format!("http://{addr}{path}");
        let (status, body) = fetch_gateway_body_with_retries(&client, &url, REQUEST_ATTEMPTS)
            .await
            .unwrap_or_else(|err| panic!("live gateway request failed for {path}: {err}"));
        assert_eq!(
            status,
            StatusCode::OK,
            "live gateway request failed for {path}: {}",
            String::from_utf8_lossy(&body)
        );
        assert!(
            !body.is_empty(),
            "live gateway returned empty body for {path}"
        );
        eprintln!("fetched {path} through local gateway: {} bytes", body.len());
    }

    let stats = stats_provider.stats();
    eprintln!(
        "retrieval transport counts: cache_hits={} http_provider_blocks={} bitswap_blocks={}",
        stats.cache_hits, stats.http_provider_blocks, stats.bitswap_blocks
    );
    assert!(
        stats.cache_hits + stats.http_provider_blocks + stats.bitswap_blocks > 0,
        "live smoke did not record any retrieval transport"
    );
}

async fn fetch_gateway_body_with_retries(
    client: &reqwest::Client,
    url: &str,
    attempts: usize,
) -> Result<(StatusCode, Vec<u8>), String> {
    let mut last_error = None;
    for attempt in 1..=attempts.max(1) {
        match client.get(url).send().await {
            Ok(response) => {
                let status = response.status();
                match response.bytes().await {
                    Ok(body) => return Ok((status, body.to_vec())),
                    Err(err) => last_error = Some(format!("response body error: {err}")),
                }
            }
            Err(err) => last_error = Some(format!("request error: {err}")),
        }
        if attempt < attempts {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Err(last_error.unwrap_or_else(|| "request was not attempted".to_string()))
}

async fn resolve_ens_contenthash(name: &str) -> String {
    let url = format!("https://api.web3.bio/profile/ens/{name}");
    let profile = reqwest::get(url)
        .await
        .expect("ENS profile request failed")
        .error_for_status()
        .expect("ENS profile request returned error")
        .json::<EnsProfile>()
        .await
        .expect("ENS profile JSON parse failed");
    let contenthash = profile
        .contenthash
        .unwrap_or_else(|| panic!("{name} has no contenthash"));
    contenthash_to_gateway_path(&contenthash)
}

fn contenthash_to_gateway_path(contenthash: &str) -> String {
    if let Some(path) = contenthash.strip_prefix("ipfs://") {
        format!("/ipfs/{}", path.trim_start_matches('/'))
    } else if let Some(path) = contenthash.strip_prefix("ipns://") {
        format!("/ipns/{}", path.trim_start_matches('/'))
    } else {
        panic!("unsupported live contenthash: {contenthash}");
    }
}

#[derive(Debug, Deserialize)]
struct EnsProfile {
    contenthash: Option<String>,
}
