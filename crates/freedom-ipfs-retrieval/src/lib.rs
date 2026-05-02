use cid::Cid;
use freedom_ipfs_core::{verify_block, Block, BlockProvider, CoreError, Result as CoreResult};
use freedom_ipfs_routing::{DelegatedRoutingClient, Provider};
use freedom_ipfs_store::SqliteBlockStore;
use thiserror::Error;
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
    #[error("no HTTP-capable providers found")]
    NoHttpProviders,
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
        Err(RetrievalError::NoHttpProviders)
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
            Err(RetrievalError::NoHttpProviders) => Ok(None),
            Err(err) => Err(CoreError::Storage(err.to_string())),
        }
    }
}
