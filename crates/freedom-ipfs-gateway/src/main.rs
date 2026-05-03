use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use freedom_ipfs_core::parse_cid;
use freedom_ipfs_gateway::{
    serve_config, serve_with_provider_and_name_resolver_config, GatewayConfig,
    DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS,
};
use freedom_ipfs_namesys::{
    CachedNameResolver, CloudflareDohResolver, DefaultNameResolver, DelegatedIpnsResolver,
    FallbackIpnsResolver, IpnsResolver,
};
use freedom_ipfs_retrieval::FetchingBlockProvider;
use freedom_ipfs_routing::{
    AutoRoutingClient, DelegatedRoutingClient, DhtIpnsResolver, LightDhtClient,
    ProviderRoutingClient, DEFAULT_DELEGATED_ROUTER, DEFAULT_DHT_QUERY_TIMEOUT,
    DEFAULT_MAX_DHT_PROVIDERS,
};
use freedom_ipfs_store::SqliteBlockStore;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(author, version, about = "Local Freedom IPFS gateway")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:0")]
    addr: SocketAddr,
    #[arg(long)]
    db: Option<PathBuf>,
    #[arg(long)]
    import_car: Option<PathBuf>,
    #[arg(long)]
    export_car: Option<PathBuf>,
    #[arg(long)]
    root: Option<String>,
    #[arg(long)]
    online: bool,
    #[arg(long, default_value = DEFAULT_DELEGATED_ROUTER)]
    delegated_router: String,
    #[arg(long, value_enum, default_value_t = RoutingMode::Auto)]
    routing_mode: RoutingMode,
    #[arg(long, default_value_t = DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS)]
    max_concurrent_requests: usize,
    #[arg(long, default_value_t = DEFAULT_DHT_QUERY_TIMEOUT.as_secs())]
    dht_query_timeout_secs: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_DHT_PROVIDERS)]
    dht_max_providers: usize,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RoutingMode {
    Auto,
    Delegated,
    LightDht,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let store = if let Some(path) = args.db {
        SqliteBlockStore::open(path, 256 * 1024 * 1024)?
    } else {
        SqliteBlockStore::in_memory(256 * 1024 * 1024)?
    };

    if let Some(car_path) = args.import_car {
        let bytes = fs::read(&car_path).with_context(|| format!("read {}", car_path.display()))?;
        let imported = store.import_car(&bytes)?;
        eprintln!("imported {} CAR blocks", imported.len());
    }

    if let Some(car_path) = args.export_car {
        let bytes = store.export_car()?;
        fs::write(&car_path, bytes).with_context(|| format!("write {}", car_path.display()))?;
        eprintln!("exported cache CAR to {}", car_path.display());
    }

    if let Some(root) = args.root {
        let root = parse_cid(&root)?;
        eprintln!("root: {root}");
    }

    let gateway_config = GatewayConfig::new(args.max_concurrent_requests);
    let bound = if args.online {
        let delegated_routers = args.delegated_router.clone();
        let delegated = delegated_routing_client(&delegated_routers);
        let dht = light_dht_client(args.dht_query_timeout_secs, args.dht_max_providers);
        let routing = match args.routing_mode {
            RoutingMode::Auto => {
                ProviderRoutingClient::from(AutoRoutingClient::new(delegated, dht.clone()))
            }
            RoutingMode::Delegated => ProviderRoutingClient::from(delegated),
            RoutingMode::LightDht => ProviderRoutingClient::from(dht.clone()),
        };
        let provider = FetchingBlockProvider::new(store, routing);
        let name_resolver = CachedNameResolver::new(DefaultNameResolver::new(
            CloudflareDohResolver::default(),
            ipns_resolver(
                args.routing_mode,
                first_delegated_router(&delegated_routers),
                dht,
            ),
        ));
        serve_with_provider_and_name_resolver_config(
            Arc::new(provider),
            Arc::new(name_resolver),
            args.addr,
            gateway_config,
        )
        .await?
    } else {
        serve_config(store, args.addr, gateway_config).await?
    };
    eprintln!("gateway listening on http://{bound}");
    Ok(())
}

fn light_dht_client(dht_query_timeout_secs: u64, dht_max_providers: usize) -> LightDhtClient {
    LightDhtClient::default()
        .with_query_timeout(Duration::from_secs(dht_query_timeout_secs))
        .with_max_providers(dht_max_providers)
}

fn delegated_routing_client(delegated_routers: &str) -> DelegatedRoutingClient {
    DelegatedRoutingClient::with_endpoints(delegated_router_endpoints(delegated_routers))
}

fn first_delegated_router(delegated_routers: &str) -> String {
    delegated_router_endpoints(delegated_routers)
        .into_iter()
        .next()
        .unwrap_or_else(|| DEFAULT_DELEGATED_ROUTER.to_string())
}

fn delegated_router_endpoints(delegated_routers: &str) -> Vec<String> {
    delegated_routers
        .split(',')
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .map(str::to_string)
        .collect()
}

fn ipns_resolver(
    routing_mode: RoutingMode,
    delegated_router: String,
    dht: LightDhtClient,
) -> Arc<dyn IpnsResolver> {
    match routing_mode {
        RoutingMode::Auto => Arc::new(FallbackIpnsResolver::new(
            DelegatedIpnsResolver::new(delegated_router),
            DhtIpnsResolver::new(dht),
        )),
        RoutingMode::Delegated => Arc::new(DelegatedIpnsResolver::new(delegated_router)),
        RoutingMode::LightDht => Arc::new(DhtIpnsResolver::new(dht)),
    }
}
