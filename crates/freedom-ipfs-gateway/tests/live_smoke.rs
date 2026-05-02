use axum::http::StatusCode;
use freedom_ipfs_gateway::router_with_provider;
use freedom_ipfs_retrieval::FetchingBlockProvider;
use freedom_ipfs_routing::{
    AutoRoutingClient, DelegatedRoutingClient, LightDhtClient, DEFAULT_DELEGATED_ROUTER,
};
use freedom_ipfs_store::SqliteBlockStore;
use serde::Deserialize;
use std::env;
use std::sync::Arc;
use tokio::net::TcpListener;

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
    let routing = AutoRoutingClient::new(
        DelegatedRoutingClient::new(router),
        LightDhtClient::default(),
    );
    let provider = FetchingBlockProvider::new(store, routing);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router_with_provider(Arc::new(provider)))
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
        let response = client.get(&url).send().await.unwrap();
        let status = response.status();
        let body = response.bytes().await.unwrap();
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
