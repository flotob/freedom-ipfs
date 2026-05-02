use anyhow::{Context, Result};
use clap::Parser;
use freedom_ipfs_core::parse_cid;
use freedom_ipfs_gateway::{serve, serve_with_provider};
use freedom_ipfs_retrieval::FetchingBlockProvider;
use freedom_ipfs_routing::{DelegatedRoutingClient, DEFAULT_DELEGATED_ROUTER};
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
    root: Option<String>,
    #[arg(long)]
    online: bool,
    #[arg(long, default_value = DEFAULT_DELEGATED_ROUTER)]
    delegated_router: String,
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

    if let Some(root) = args.root {
        let root = parse_cid(&root)?;
        eprintln!("root: {root}");
    }

    let bound = if args.online {
        let routing = DelegatedRoutingClient::new(args.delegated_router);
        let provider = FetchingBlockProvider::new(store, routing);
        serve_with_provider(Arc::new(provider), args.addr).await?
    } else {
        serve(store, args.addr).await?
    };
    eprintln!("gateway listening on http://{bound}");
    Ok(())
}
