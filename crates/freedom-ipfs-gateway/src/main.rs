use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use freedom_ipfs_core::parse_cid;
use freedom_ipfs_gateway::{
    serve_config, serve_with_provider_config, GatewayConfig,
    DEFAULT_GATEWAY_MAX_CONCURRENT_REQUESTS,
};
use freedom_ipfs_retrieval::FetchingBlockProvider;
use freedom_ipfs_routing::{
    AutoRoutingClient, DelegatedRoutingClient, LightDhtClient, ProviderRoutingClient,
    DEFAULT_DELEGATED_ROUTER,
};
use freedom_ipfs_store::SqliteBlockStore;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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
        let delegated = DelegatedRoutingClient::new(args.delegated_router);
        let routing = match args.routing_mode {
            RoutingMode::Auto => ProviderRoutingClient::from(AutoRoutingClient::new(
                delegated,
                LightDhtClient::default(),
            )),
            RoutingMode::Delegated => ProviderRoutingClient::from(delegated),
            RoutingMode::LightDht => ProviderRoutingClient::from(LightDhtClient::default()),
        };
        let provider = FetchingBlockProvider::new(store, routing);
        serve_with_provider_config(Arc::new(provider), args.addr, gateway_config).await?
    } else {
        serve_config(store, args.addr, gateway_config).await?
    };
    eprintln!("gateway listening on http://{bound}");
    Ok(())
}
