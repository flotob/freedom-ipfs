use cid::Cid;
use freedom_ipfs_core::{BlockProvider, CODEC_DAG_PB, CODEC_RAW};
use multihash::Multihash;
use prost::Message;
use std::io::Cursor;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UnixfsError {
    #[error("block not found: {0}")]
    NotFound(Cid),
    #[error("path segment not found: {0}")]
    PathNotFound(String),
    #[error("unsupported codec {0}")]
    UnsupportedCodec(u64),
    #[error("unsupported unixfs node type {0}")]
    UnsupportedNodeType(i32),
    #[error("path resolves to a directory")]
    IsDirectory,
    #[error("path requires a directory but found file")]
    NotDirectory,
    #[error("invalid dag-pb: {0}")]
    InvalidDagPb(String),
    #[error("block provider: {0}")]
    Provider(String),
}

pub type Result<T> = std::result::Result<T, UnixfsError>;

#[derive(Clone, PartialEq, Message)]
struct PbNode {
    #[prost(bytes = "vec", optional, tag = "1")]
    data: Option<Vec<u8>>,
    #[prost(message, repeated, tag = "2")]
    links: Vec<PbLink>,
}

#[derive(Clone, PartialEq, Message)]
struct PbLink {
    #[prost(bytes = "vec", optional, tag = "1")]
    hash: Option<Vec<u8>>,
    #[prost(string, optional, tag = "2")]
    name: Option<String>,
    #[prost(uint64, optional, tag = "3")]
    tsize: Option<u64>,
}

#[derive(Clone, PartialEq, Message)]
struct UnixfsData {
    #[prost(enumeration = "DataType", optional, tag = "1")]
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
enum DataType {
    Raw = 0,
    Directory = 1,
    File = 2,
    Metadata = 3,
    Symlink = 4,
    HamtShard = 5,
}

#[derive(Debug, Clone)]
pub struct ResolvedNode {
    pub cid: Cid,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Raw,
    File,
    Directory,
    HamtShard,
}

pub fn resolve_path(provider: &dyn BlockProvider, root: &Cid, path: &str) -> Result<ResolvedNode> {
    let mut current = *root;
    let mut segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .peekable();

    if segments.peek().is_none() {
        return classify(provider, &current);
    }

    for segment in segments {
        let block = provider
            .get_block(&current)
            .map_err(|err| UnixfsError::Provider(err.to_string()))?
            .ok_or(UnixfsError::NotFound(current))?;

        if block.codec() != CODEC_DAG_PB {
            return Err(UnixfsError::NotDirectory);
        }

        let node = decode_pb_node(block.data())?;
        let data = decode_unixfs_data(&node)?;
        match data_type(&data)? {
            DataType::Directory => {
                current = find_link(&node, segment)?
                    .ok_or_else(|| UnixfsError::PathNotFound(segment.to_string()))?;
            }
            DataType::HamtShard => {
                return Err(UnixfsError::UnsupportedNodeType(DataType::HamtShard as i32))
            }
            _ => return Err(UnixfsError::NotDirectory),
        }
    }

    classify(provider, &current)
}

pub fn read_file(provider: &dyn BlockProvider, root: &Cid, path: &str) -> Result<Vec<u8>> {
    let resolved = resolve_path(provider, root, path)?;
    match resolved.kind {
        NodeKind::Directory | NodeKind::HamtShard => return Err(UnixfsError::IsDirectory),
        NodeKind::Raw | NodeKind::File => {}
    }
    read_file_cid(provider, &resolved.cid)
}

fn read_file_cid(provider: &dyn BlockProvider, cid: &Cid) -> Result<Vec<u8>> {
    let block = provider
        .get_block(cid)
        .map_err(|err| UnixfsError::Provider(err.to_string()))?
        .ok_or(UnixfsError::NotFound(*cid))?;

    match block.codec() {
        CODEC_RAW => Ok(block.data().to_vec()),
        CODEC_DAG_PB => {
            let node = decode_pb_node(block.data())?;
            let data = decode_unixfs_data(&node)?;
            match data_type(&data)? {
                DataType::Raw | DataType::File => {
                    let mut out = data.data.unwrap_or_default();
                    for link in &node.links {
                        let child = link_cid(link)?;
                        out.extend_from_slice(&read_file_cid(provider, &child)?);
                    }
                    Ok(out)
                }
                DataType::Directory | DataType::HamtShard => Err(UnixfsError::IsDirectory),
                other => Err(UnixfsError::UnsupportedNodeType(other as i32)),
            }
        }
        codec => Err(UnixfsError::UnsupportedCodec(codec)),
    }
}

fn classify(provider: &dyn BlockProvider, cid: &Cid) -> Result<ResolvedNode> {
    let block = provider
        .get_block(cid)
        .map_err(|err| UnixfsError::Provider(err.to_string()))?
        .ok_or(UnixfsError::NotFound(*cid))?;

    let kind = match block.codec() {
        CODEC_RAW => NodeKind::Raw,
        CODEC_DAG_PB => {
            let node = decode_pb_node(block.data())?;
            let data = decode_unixfs_data(&node)?;
            match data_type(&data)? {
                DataType::Raw | DataType::File => NodeKind::File,
                DataType::Directory => NodeKind::Directory,
                DataType::HamtShard => NodeKind::HamtShard,
                other => return Err(UnixfsError::UnsupportedNodeType(other as i32)),
            }
        }
        codec => return Err(UnixfsError::UnsupportedCodec(codec)),
    };

    Ok(ResolvedNode { cid: *cid, kind })
}

fn decode_pb_node(data: &[u8]) -> Result<PbNode> {
    PbNode::decode(data).map_err(|err| UnixfsError::InvalidDagPb(err.to_string()))
}

fn decode_unixfs_data(node: &PbNode) -> Result<UnixfsData> {
    let data = node
        .data
        .as_deref()
        .ok_or_else(|| UnixfsError::InvalidDagPb("missing UnixFS data".into()))?;
    UnixfsData::decode(data).map_err(|err| UnixfsError::InvalidDagPb(err.to_string()))
}

fn data_type(data: &UnixfsData) -> Result<DataType> {
    let value = data.r#type.unwrap_or(DataType::Raw as i32);
    DataType::try_from(value).map_err(|_| UnixfsError::UnsupportedNodeType(value))
}

fn find_link(node: &PbNode, name: &str) -> Result<Option<Cid>> {
    node.links
        .iter()
        .find(|link| link.name.as_deref() == Some(name))
        .map(link_cid)
        .transpose()
}

fn link_cid(link: &PbLink) -> Result<Cid> {
    let bytes = link
        .hash
        .as_deref()
        .ok_or_else(|| UnixfsError::InvalidDagPb("link is missing hash".into()))?;

    let mut cursor = Cursor::new(bytes);
    if let Ok(cid) = Cid::read_bytes(&mut cursor) {
        if cursor.position() as usize == bytes.len() {
            return Ok(cid);
        }
    }

    let mh = Multihash::<64>::from_bytes(bytes)
        .map_err(|err| UnixfsError::InvalidDagPb(format!("invalid link multihash: {err}")))?;
    Ok(Cid::new_v1(CODEC_DAG_PB, mh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use freedom_ipfs_core::{cid_from_data, CODEC_DAG_PB, CODEC_RAW};
    use freedom_ipfs_store::SqliteBlockStore;

    fn unixfs_data(kind: DataType, data: &[u8]) -> Vec<u8> {
        UnixfsData {
            r#type: Some(kind as i32),
            data: Some(data.to_vec()),
            filesize: Some(data.len() as u64),
            blocksizes: Vec::new(),
        }
        .encode_to_vec()
    }

    fn pb_file(data: &[u8], links: Vec<PbLink>) -> Vec<u8> {
        PbNode {
            data: Some(unixfs_data(DataType::File, data)),
            links,
        }
        .encode_to_vec()
    }

    fn pb_directory(links: Vec<PbLink>) -> Vec<u8> {
        PbNode {
            data: Some(unixfs_data(DataType::Directory, &[])),
            links,
        }
        .encode_to_vec()
    }

    fn link(name: &str, cid: &Cid) -> PbLink {
        PbLink {
            hash: Some(cid.to_bytes()),
            name: Some(name.to_string()),
            tsize: None,
        }
    }

    #[test]
    fn reads_raw_block_as_file() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"raw leaf";
        let cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&cid, data).unwrap();
        assert_eq!(read_file(&store, &cid, "").unwrap(), data);
    }

    #[test]
    fn resolves_directory_and_reads_linked_file() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();

        let leaf_data = b"linked bytes";
        let leaf_cid = cid_from_data(CODEC_RAW, leaf_data);
        store.put_block(&leaf_cid, leaf_data).unwrap();

        let file_data = pb_file(b"prefix ", vec![link("leaf", &leaf_cid)]);
        let file_cid = cid_from_data(CODEC_DAG_PB, &file_data);
        store.put_block(&file_cid, &file_data).unwrap();

        let dir_data = pb_directory(vec![link("index.html", &file_cid)]);
        let dir_cid = cid_from_data(CODEC_DAG_PB, &dir_data);
        store.put_block(&dir_cid, &dir_data).unwrap();

        assert_eq!(
            read_file(&store, &dir_cid, "index.html").unwrap(),
            b"prefix linked bytes"
        );
    }
}
