use cid::Cid;
use freedom_ipfs_core::{
    encode_car_v1, parse_car_v1, verify_block, Block, BlockProvider, CarBlock, CoreError,
    Result as CoreResult,
};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

const DEFAULT_CACHE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("core: {0}")]
    Core(#[from] CoreError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone)]
pub struct SqliteBlockStore {
    conn: Arc<Mutex<Connection>>,
    max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedProviderRecord {
    pub id: Option<String>,
    pub addrs: Vec<String>,
}

impl SqliteBlockStore {
    pub fn open(path: impl AsRef<Path>, max_bytes: u64) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            max_bytes: if max_bytes == 0 {
                DEFAULT_CACHE_BYTES
            } else {
                max_bytes
            },
        };
        store.init()?;
        Ok(store)
    }

    pub fn in_memory(max_bytes: u64) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            max_bytes: if max_bytes == 0 {
                DEFAULT_CACHE_BYTES
            } else {
                max_bytes
            },
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        self.conn.lock().execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            CREATE TABLE IF NOT EXISTS blocks (
                cid BLOB PRIMARY KEY NOT NULL,
                codec INTEGER NOT NULL,
                size INTEGER NOT NULL,
                data BLOB NOT NULL,
                inserted_at INTEGER NOT NULL,
                last_accessed_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS blocks_last_accessed
                ON blocks(last_accessed_at);
            CREATE TABLE IF NOT EXISTS bad_providers (
                peer_or_url TEXT PRIMARY KEY NOT NULL,
                reason TEXT NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS provider_cache (
                cid BLOB PRIMARY KEY NOT NULL,
                providers_json TEXT NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn put_block(&self, cid: &Cid, data: &[u8]) -> Result<()> {
        verify_block(cid, data)?;
        let now = now_secs();
        self.conn.lock().execute(
            r#"
            INSERT INTO blocks(cid, codec, size, data, inserted_at, last_accessed_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(cid) DO UPDATE SET
                codec = excluded.codec,
                size = excluded.size,
                data = excluded.data,
                last_accessed_at = excluded.last_accessed_at
            "#,
            params![
                cid.to_bytes(),
                cid.codec() as i64,
                data.len() as i64,
                data,
                now as i64
            ],
        )?;
        self.evict_if_needed()?;
        Ok(())
    }

    pub fn put(&self, block: &Block) -> Result<()> {
        self.put_block(block.cid(), block.data())
    }

    pub fn get(&self, cid: &Cid) -> Result<Option<Block>> {
        let cid_bytes = cid.to_bytes();
        let row = self
            .conn
            .lock()
            .query_row(
                "SELECT data FROM blocks WHERE cid = ?1",
                params![cid_bytes],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;

        match row {
            Some(data) => {
                verify_block(cid, &data)?;
                self.touch(cid)?;
                Ok(Some(Block::unchecked(*cid, data)))
            }
            None => Ok(None),
        }
    }

    pub fn import_car(&self, bytes: &[u8]) -> Result<Vec<Cid>> {
        let car = parse_car_v1(bytes)?;
        let mut imported = Vec::with_capacity(car.blocks.len());
        for block in car.blocks {
            self.put_block(&block.cid, &block.data)?;
            imported.push(block.cid);
        }
        Ok(imported)
    }

    pub fn export_car(&self) -> Result<Vec<u8>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT cid, data FROM blocks ORDER BY inserted_at ASC")?;
        let blocks = stmt
            .query_map([], |row| {
                let cid_bytes = row.get::<_, Vec<u8>>(0)?;
                let data = row.get::<_, Vec<u8>>(1)?;
                Ok((cid_bytes, data))
            })?
            .map(|row| {
                let (cid_bytes, data) = row?;
                let cid = Cid::read_bytes(&mut Cursor::new(cid_bytes))
                    .map_err(|err| CoreError::InvalidCid(err.to_string()))?;
                Ok(CarBlock { cid, data })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(encode_car_v1(&blocks))
    }

    pub fn put_provider_records(
        &self,
        cid: &Cid,
        providers: &[CachedProviderRecord],
        ttl: Duration,
    ) -> Result<()> {
        if providers.is_empty() {
            return Ok(());
        }
        let expires_at = now_secs().saturating_add(ttl.as_secs());
        let providers_json = serde_json::to_string(providers)?;
        self.conn.lock().execute(
            r#"
            INSERT INTO provider_cache(cid, providers_json, expires_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(cid) DO UPDATE SET
                providers_json = excluded.providers_json,
                expires_at = excluded.expires_at
            "#,
            params![provider_cache_key(cid), providers_json, expires_at as i64],
        )?;
        Ok(())
    }

    pub fn get_provider_records(&self, cid: &Cid) -> Result<Option<Vec<CachedProviderRecord>>> {
        let cid_bytes = provider_cache_key(cid);
        let row = self
            .conn
            .lock()
            .query_row(
                "SELECT providers_json, expires_at FROM provider_cache WHERE cid = ?1",
                params![cid_bytes],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;

        let Some((providers_json, expires_at)) = row else {
            return Ok(None);
        };
        if expires_at <= now_secs() as i64 {
            self.conn.lock().execute(
                "DELETE FROM provider_cache WHERE cid = ?1",
                params![provider_cache_key(cid)],
            )?;
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&providers_json)?))
    }

    pub fn mark_bad_provider(&self, peer_or_url: &str, reason: &str, ttl: Duration) -> Result<()> {
        let expires_at = now_secs().saturating_add(ttl.as_secs());
        self.conn.lock().execute(
            r#"
            INSERT INTO bad_providers(peer_or_url, reason, expires_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(peer_or_url) DO UPDATE SET
                reason = excluded.reason,
                expires_at = excluded.expires_at
            "#,
            params![peer_or_url, reason, expires_at as i64],
        )?;
        Ok(())
    }

    pub fn is_bad_provider(&self, peer_or_url: &str) -> Result<bool> {
        let expires_at = self
            .conn
            .lock()
            .query_row(
                "SELECT expires_at FROM bad_providers WHERE peer_or_url = ?1",
                params![peer_or_url],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(expires_at) = expires_at else {
            return Ok(false);
        };
        if expires_at <= now_secs() as i64 {
            self.conn.lock().execute(
                "DELETE FROM bad_providers WHERE peer_or_url = ?1",
                params![peer_or_url],
            )?;
            return Ok(false);
        }
        Ok(true)
    }

    pub fn total_bytes(&self) -> Result<u64> {
        let total =
            self.conn
                .lock()
                .query_row("SELECT COALESCE(SUM(size), 0) FROM blocks", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        Ok(total.max(0) as u64)
    }

    pub fn block_count(&self) -> Result<u64> {
        let count = self
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM blocks", [], |row| {
                row.get::<_, i64>(0)
            })?;
        Ok(count.max(0) as u64)
    }

    pub fn clear(&self) -> Result<()> {
        self.conn.lock().execute("DELETE FROM blocks", [])?;
        self.conn.lock().execute("DELETE FROM provider_cache", [])?;
        self.conn.lock().execute("DELETE FROM bad_providers", [])?;
        Ok(())
    }

    pub fn trim_blocks_to(&self, max_bytes: u64) -> Result<()> {
        self.evict_until(max_bytes)
    }

    fn touch(&self, cid: &Cid) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE blocks SET last_accessed_at = ?1 WHERE cid = ?2",
            params![now_secs() as i64, cid.to_bytes()],
        )?;
        Ok(())
    }

    fn evict_if_needed(&self) -> Result<()> {
        self.evict_until(self.max_bytes)
    }

    fn evict_until(&self, max_bytes: u64) -> Result<()> {
        loop {
            let total = self.total_bytes()?;
            if total <= max_bytes {
                return Ok(());
            }
            let deleted = self.conn.lock().execute(
                r#"
                DELETE FROM blocks
                WHERE cid = (
                    SELECT cid FROM blocks
                    ORDER BY last_accessed_at ASC, inserted_at ASC
                    LIMIT 1
                )
                "#,
                [],
            )?;
            if deleted == 0 {
                return Ok(());
            }
        }
    }
}

impl BlockProvider for SqliteBlockStore {
    fn get_block(&self, cid: &Cid) -> CoreResult<Option<Block>> {
        self.get(cid)
            .map_err(|err| CoreError::Storage(err.to_string()))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn provider_cache_key(cid: &Cid) -> Vec<u8> {
    cid.hash().to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use freedom_ipfs_core::{cid_from_data, CODEC_RAW};

    #[test]
    fn stores_and_verifies_blocks() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"cached block";
        let cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();

        let block = store.get(&cid).unwrap().unwrap();
        assert_eq!(block.data(), data);
        assert_eq!(block.cid(), &cid);
        assert_eq!(store.block_count().unwrap(), 1);
    }

    #[test]
    fn evicts_lru_blocks_when_over_budget() {
        let store = SqliteBlockStore::in_memory(20).unwrap();
        let first = vec![1u8; 16];
        let second = vec![2u8; 16];
        let first_cid = cid_from_data(CODEC_RAW, &first);
        let second_cid = cid_from_data(CODEC_RAW, &second);
        store.put_block(&first_cid, &first).unwrap();
        store.put_block(&second_cid, &second).unwrap();

        assert!(store.total_bytes().unwrap() <= 20);
        assert_eq!(store.get(&second_cid).unwrap().unwrap().data(), second);
    }

    #[test]
    fn trims_blocks_to_requested_budget() {
        let store = SqliteBlockStore::in_memory(1024).unwrap();
        let first = vec![1u8; 16];
        let second = vec![2u8; 16];
        let first_cid = cid_from_data(CODEC_RAW, &first);
        let second_cid = cid_from_data(CODEC_RAW, &second);
        store.put_block(&first_cid, &first).unwrap();
        store.put_block(&second_cid, &second).unwrap();

        store.trim_blocks_to(20).unwrap();

        assert!(store.total_bytes().unwrap() <= 20);
        assert_eq!(store.block_count().unwrap(), 1);
    }

    #[test]
    fn exports_cache_as_importable_car() {
        let source = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"car export block";
        let cid = cid_from_data(CODEC_RAW, data);
        source.put_block(&cid, data).unwrap();

        let car = source.export_car().unwrap();
        let target = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let imported = target.import_car(&car).unwrap();

        assert_eq!(imported, vec![cid]);
        assert_eq!(target.get(&cid).unwrap().unwrap().data(), data);
    }

    #[test]
    fn caches_provider_records_until_ttl_expires() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let cid = cid_from_data(CODEC_RAW, b"provider cache key");
        let providers = vec![CachedProviderRecord {
            id: Some("peer".to_string()),
            addrs: vec!["/ip4/127.0.0.1/tcp/4001".to_string()],
        }];

        store
            .put_provider_records(&cid, &providers, Duration::from_secs(60))
            .unwrap();
        assert_eq!(store.get_provider_records(&cid).unwrap(), Some(providers));

        store
            .put_provider_records(
                &cid,
                &[CachedProviderRecord {
                    id: None,
                    addrs: vec![],
                }],
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(store.get_provider_records(&cid).unwrap(), None);
    }

    #[test]
    fn provider_cache_key_is_cid_representation_independent() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let cidv1 = cid_from_data(freedom_ipfs_core::CODEC_DAG_PB, b"provider multihash key");
        let cidv0 = Cid::new_v0(*cidv1.hash()).unwrap();
        let providers = vec![CachedProviderRecord {
            id: Some("peer".to_string()),
            addrs: vec!["/ip4/127.0.0.1/tcp/4001".to_string()],
        }];

        store
            .put_provider_records(&cidv1, &providers, Duration::from_secs(60))
            .unwrap();

        assert_eq!(store.get_provider_records(&cidv0).unwrap(), Some(providers));
    }

    #[test]
    fn tracks_bad_providers_until_ttl_expires() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        store
            .mark_bad_provider("peer", "timeout", Duration::from_secs(60))
            .unwrap();
        assert!(store.is_bad_provider("peer").unwrap());

        store
            .mark_bad_provider("peer", "timeout", Duration::ZERO)
            .unwrap();
        assert!(!store.is_bad_provider("peer").unwrap());
    }
}
