use cid::Cid;
use freedom_ipfs_core::{
    parse_car_v1, verify_block, Block, BlockProvider, CoreError, Result as CoreResult,
};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

const DEFAULT_CACHE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("core: {0}")]
    Core(#[from] CoreError),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone)]
pub struct SqliteBlockStore {
    conn: Arc<Mutex<Connection>>,
    max_bytes: u64,
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
        Ok(())
    }

    fn touch(&self, cid: &Cid) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE blocks SET last_accessed_at = ?1 WHERE cid = ?2",
            params![now_secs() as i64, cid.to_bytes()],
        )?;
        Ok(())
    }

    fn evict_if_needed(&self) -> Result<()> {
        loop {
            let total = self.total_bytes()?;
            if total <= self.max_bytes {
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
}
