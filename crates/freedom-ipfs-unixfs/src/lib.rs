use cid::Cid;
use freedom_ipfs_core::{BlockProvider, CODEC_DAG_PB, CODEC_RAW};
use multihash::Multihash;
use prost::Message;
use std::io::Cursor;
use thiserror::Error;

const HAMT_MURMUR3_X64_64: u64 = 0x22;
const HAMT_FANOUT_256: u64 = 256;
const HAMT_LINK_PREFIX_LEN: usize = 2;
const HAMT_MAX_SHARDS_VISITED: usize = 1024;

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
    #[prost(uint64, optional, tag = "5")]
    hash_type: Option<u64>,
    #[prost(uint64, optional, tag = "6")]
    fanout: Option<u64>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub cid: Cid,
    pub size: Option<u64>,
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
                current = find_hamt_link(provider, &node, &data, segment)?
                    .ok_or_else(|| UnixfsError::PathNotFound(segment.to_string()))?;
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

pub fn list_directory(
    provider: &dyn BlockProvider,
    root: &Cid,
    path: &str,
) -> Result<Vec<DirectoryEntry>> {
    let resolved = resolve_path(provider, root, path)?;
    match resolved.kind {
        NodeKind::Directory | NodeKind::HamtShard => list_directory_cid(provider, &resolved.cid),
        NodeKind::Raw | NodeKind::File => Err(UnixfsError::NotDirectory),
    }
}

pub fn file_size(provider: &dyn BlockProvider, root: &Cid, path: &str) -> Result<u64> {
    let resolved = resolve_path(provider, root, path)?;
    match resolved.kind {
        NodeKind::Directory | NodeKind::HamtShard => return Err(UnixfsError::IsDirectory),
        NodeKind::Raw | NodeKind::File => {}
    }
    file_size_cid(provider, &resolved.cid)
}

pub fn read_file_range(
    provider: &dyn BlockProvider,
    root: &Cid,
    path: &str,
    start: u64,
    end: u64,
) -> Result<Vec<u8>> {
    let resolved = resolve_path(provider, root, path)?;
    match resolved.kind {
        NodeKind::Directory | NodeKind::HamtShard => return Err(UnixfsError::IsDirectory),
        NodeKind::Raw | NodeKind::File => {}
    }
    read_file_cid_range(provider, &resolved.cid, start, end)
}

fn list_directory_cid(provider: &dyn BlockProvider, cid: &Cid) -> Result<Vec<DirectoryEntry>> {
    let block = provider
        .get_block(cid)
        .map_err(|err| UnixfsError::Provider(err.to_string()))?
        .ok_or(UnixfsError::NotFound(*cid))?;
    if block.codec() != CODEC_DAG_PB {
        return Err(UnixfsError::NotDirectory);
    }

    let node = decode_pb_node(block.data())?;
    let data = decode_unixfs_data(&node)?;
    let mut entries = match data_type(&data)? {
        DataType::Directory => directory_entries(&node.links)?,
        DataType::HamtShard => hamt_directory_entries(provider, &node, &data)?,
        _ => return Err(UnixfsError::NotDirectory),
    };
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn directory_entries(links: &[PbLink]) -> Result<Vec<DirectoryEntry>> {
    let mut entries = Vec::new();
    for link in links {
        let Some(name) = link.name.as_ref().filter(|name| !name.is_empty()) else {
            continue;
        };
        entries.push(DirectoryEntry {
            name: name.clone(),
            cid: link_cid(link)?,
            size: link.tsize,
        });
    }
    Ok(entries)
}

fn hamt_directory_entries(
    provider: &dyn BlockProvider,
    node: &PbNode,
    data: &UnixfsData,
) -> Result<Vec<DirectoryEntry>> {
    validate_hamt(data)?;
    let mut entries = Vec::new();
    let mut pending = Vec::new();
    collect_hamt_entries(&node.links, &mut entries, &mut pending)?;

    let mut visited = 0usize;
    while let Some(cid) = pending.pop() {
        visited += 1;
        if visited > HAMT_MAX_SHARDS_VISITED {
            return Err(UnixfsError::InvalidDagPb(format!(
                "HAMT traversal exceeded {HAMT_MAX_SHARDS_VISITED} shards"
            )));
        }

        let block = provider
            .get_block(&cid)
            .map_err(|err| UnixfsError::Provider(err.to_string()))?
            .ok_or(UnixfsError::NotFound(cid))?;
        if block.codec() != CODEC_DAG_PB {
            return Err(UnixfsError::NotDirectory);
        }
        let shard = decode_pb_node(block.data())?;
        let shard_data = decode_unixfs_data(&shard)?;
        if data_type(&shard_data)? != DataType::HamtShard {
            return Err(UnixfsError::InvalidDagPb(
                "HAMT bucket link did not resolve to a HAMT shard".into(),
            ));
        }
        validate_hamt(&shard_data)?;
        collect_hamt_entries(&shard.links, &mut entries, &mut pending)?;
    }

    Ok(entries)
}

fn collect_hamt_entries(
    links: &[PbLink],
    entries: &mut Vec<DirectoryEntry>,
    pending: &mut Vec<Cid>,
) -> Result<()> {
    for link in links {
        let Some(link_name) = link.name.as_deref() else {
            continue;
        };
        let link_name = link_name.as_bytes();
        if link_name.len() == HAMT_LINK_PREFIX_LEN {
            pending.push(link_cid(link)?);
        } else if link_name.len() > HAMT_LINK_PREFIX_LEN {
            entries.push(DirectoryEntry {
                name: String::from_utf8_lossy(&link_name[HAMT_LINK_PREFIX_LEN..]).to_string(),
                cid: link_cid(link)?,
                size: link.tsize,
            });
        }
    }
    Ok(())
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

fn file_size_cid(provider: &dyn BlockProvider, cid: &Cid) -> Result<u64> {
    let block = provider
        .get_block(cid)
        .map_err(|err| UnixfsError::Provider(err.to_string()))?
        .ok_or(UnixfsError::NotFound(*cid))?;

    match block.codec() {
        CODEC_RAW => Ok(block.data().len() as u64),
        CODEC_DAG_PB => {
            let node = decode_pb_node(block.data())?;
            let data = decode_unixfs_data(&node)?;
            match data_type(&data)? {
                DataType::Raw | DataType::File => {
                    if let Some(filesize) = data.filesize {
                        return Ok(filesize);
                    }
                    let mut size = data.data.as_ref().map_or(0, |bytes| bytes.len() as u64);
                    for link in &node.links {
                        size = size.saturating_add(file_size_cid(provider, &link_cid(link)?)?);
                    }
                    Ok(size)
                }
                DataType::Directory | DataType::HamtShard => Err(UnixfsError::IsDirectory),
                other => Err(UnixfsError::UnsupportedNodeType(other as i32)),
            }
        }
        codec => Err(UnixfsError::UnsupportedCodec(codec)),
    }
}

fn read_file_cid_range(
    provider: &dyn BlockProvider,
    cid: &Cid,
    start: u64,
    end: u64,
) -> Result<Vec<u8>> {
    let block = provider
        .get_block(cid)
        .map_err(|err| UnixfsError::Provider(err.to_string()))?
        .ok_or(UnixfsError::NotFound(*cid))?;

    match block.codec() {
        CODEC_RAW => Ok(slice_bytes(block.data(), start, end)),
        CODEC_DAG_PB => {
            let node = decode_pb_node(block.data())?;
            let data = decode_unixfs_data(&node)?;
            match data_type(&data)? {
                DataType::Raw | DataType::File => {
                    let mut out = Vec::new();
                    let mut offset = 0u64;
                    if let Some(inline) = data.data.as_deref() {
                        append_intersection(&mut out, inline, offset, start, end);
                        offset = offset.saturating_add(inline.len() as u64);
                    }

                    for (index, link) in node.links.iter().enumerate() {
                        let child = link_cid(link)?;
                        let child_size = data
                            .blocksizes
                            .get(index)
                            .copied()
                            .map(Ok)
                            .unwrap_or_else(|| file_size_cid(provider, &child))?;
                        if child_size == 0 {
                            continue;
                        }
                        let child_end = offset.saturating_add(child_size - 1);
                        if ranges_intersect(offset, child_end, start, end) {
                            let range_start = start.saturating_sub(offset);
                            let range_end = end.min(child_end).saturating_sub(offset);
                            out.extend_from_slice(&read_file_cid_range(
                                provider,
                                &child,
                                range_start,
                                range_end,
                            )?);
                        }
                        offset = offset.saturating_add(child_size);
                        if offset > end {
                            break;
                        }
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

fn append_intersection(out: &mut Vec<u8>, bytes: &[u8], offset: u64, start: u64, end: u64) {
    if bytes.is_empty() {
        return;
    }
    let block_end = offset.saturating_add(bytes.len() as u64 - 1);
    if !ranges_intersect(offset, block_end, start, end) {
        return;
    }
    out.extend_from_slice(&slice_bytes(
        bytes,
        start.saturating_sub(offset),
        end.min(block_end).saturating_sub(offset),
    ));
}

fn slice_bytes(bytes: &[u8], start: u64, end: u64) -> Vec<u8> {
    if bytes.is_empty() || start > end || start >= bytes.len() as u64 {
        return Vec::new();
    }
    let start = start.min(bytes.len() as u64) as usize;
    let end = end.min(bytes.len() as u64 - 1) as usize;
    bytes[start..=end].to_vec()
}

fn ranges_intersect(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start <= b_end && b_start <= a_end
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

fn find_hamt_link(
    provider: &dyn BlockProvider,
    node: &PbNode,
    data: &UnixfsData,
    name: &str,
) -> Result<Option<Cid>> {
    validate_hamt(data)?;
    let mut pending = Vec::new();
    if let Some(cid) = scan_hamt_links(&node.links, name, &mut pending)? {
        return Ok(Some(cid));
    }

    let mut visited = 0usize;
    while let Some(cid) = pending.pop() {
        visited += 1;
        if visited > HAMT_MAX_SHARDS_VISITED {
            return Err(UnixfsError::InvalidDagPb(format!(
                "HAMT traversal exceeded {HAMT_MAX_SHARDS_VISITED} shards"
            )));
        }

        let block = provider
            .get_block(&cid)
            .map_err(|err| UnixfsError::Provider(err.to_string()))?
            .ok_or(UnixfsError::NotFound(cid))?;
        if block.codec() != CODEC_DAG_PB {
            return Err(UnixfsError::NotDirectory);
        }

        let shard = decode_pb_node(block.data())?;
        let shard_data = decode_unixfs_data(&shard)?;
        if data_type(&shard_data)? != DataType::HamtShard {
            return Err(UnixfsError::InvalidDagPb(
                "HAMT bucket link did not resolve to a HAMT shard".into(),
            ));
        }
        validate_hamt(&shard_data)?;
        if let Some(cid) = scan_hamt_links(&shard.links, name, &mut pending)? {
            return Ok(Some(cid));
        }
    }

    Ok(None)
}

fn validate_hamt(data: &UnixfsData) -> Result<()> {
    if data.hash_type != Some(HAMT_MURMUR3_X64_64) || data.fanout != Some(HAMT_FANOUT_256) {
        return Err(UnixfsError::InvalidDagPb(format!(
            "unsupported HAMT parameters hashType={:?} fanout={:?}",
            data.hash_type, data.fanout
        )));
    }
    if data.filesize.is_some() || !data.blocksizes.is_empty() {
        return Err(UnixfsError::InvalidDagPb(
            "HAMT shard carried file-only UnixFS fields".into(),
        ));
    }
    Ok(())
}

fn scan_hamt_links(links: &[PbLink], name: &str, pending: &mut Vec<Cid>) -> Result<Option<Cid>> {
    for link in links {
        let Some(link_name) = link.name.as_deref() else {
            continue;
        };
        let link_name = link_name.as_bytes();
        if link_name.len() == HAMT_LINK_PREFIX_LEN {
            pending.push(link_cid(link)?);
        } else if link_name.len() > HAMT_LINK_PREFIX_LEN
            && &link_name[HAMT_LINK_PREFIX_LEN..] == name.as_bytes()
        {
            return link_cid(link).map(Some);
        }
    }
    Ok(None)
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
            hash_type: None,
            fanout: None,
        }
        .encode_to_vec()
    }

    fn pb_file(data: &[u8], links: Vec<PbLink>) -> Vec<u8> {
        pb_file_with_metadata(data, links, data.len() as u64, Vec::new())
    }

    fn pb_file_with_metadata(
        data: &[u8],
        links: Vec<PbLink>,
        filesize: u64,
        blocksizes: Vec<u64>,
    ) -> Vec<u8> {
        PbNode {
            data: Some(
                UnixfsData {
                    r#type: Some(DataType::File as i32),
                    data: Some(data.to_vec()),
                    filesize: Some(filesize),
                    blocksizes,
                    hash_type: None,
                    fanout: None,
                }
                .encode_to_vec(),
            ),
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

    fn pb_hamt(links: Vec<PbLink>) -> Vec<u8> {
        PbNode {
            data: Some(
                UnixfsData {
                    r#type: Some(DataType::HamtShard as i32),
                    data: Some(vec![0xff; (HAMT_FANOUT_256 / 8) as usize]),
                    filesize: None,
                    blocksizes: Vec::new(),
                    hash_type: Some(HAMT_MURMUR3_X64_64),
                    fanout: Some(HAMT_FANOUT_256),
                }
                .encode_to_vec(),
            ),
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

    #[test]
    fn lists_directory_entries_without_reading_child_blocks() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();

        let alpha_data = b"alpha";
        let alpha_cid = cid_from_data(CODEC_RAW, alpha_data);
        let beta_data = b"beta";
        let beta_cid = cid_from_data(CODEC_RAW, beta_data);

        let dir_data = pb_directory(vec![
            PbLink {
                tsize: Some(5),
                ..link("beta.txt", &beta_cid)
            },
            PbLink {
                tsize: Some(4),
                ..link("alpha.txt", &alpha_cid)
            },
        ]);
        let dir_cid = cid_from_data(CODEC_DAG_PB, &dir_data);
        store.put_block(&dir_cid, &dir_data).unwrap();

        let entries = list_directory(&store, &dir_cid, "").unwrap();

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.cid, entry.size))
                .collect::<Vec<_>>(),
            vec![
                ("alpha.txt", alpha_cid, Some(4)),
                ("beta.txt", beta_cid, Some(5))
            ]
        );
    }

    #[test]
    fn reads_file_range_across_inline_and_linked_blocks() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();

        let leaf_data = b"linked bytes";
        let leaf_cid = cid_from_data(CODEC_RAW, leaf_data);
        store.put_block(&leaf_cid, leaf_data).unwrap();

        let file_data = pb_file_with_metadata(
            b"prefix ",
            vec![link("leaf", &leaf_cid)],
            19,
            vec![leaf_data.len() as u64],
        );
        let file_cid = cid_from_data(CODEC_DAG_PB, &file_data);
        store.put_block(&file_cid, &file_data).unwrap();

        assert_eq!(file_size(&store, &file_cid, "").unwrap(), 19);
        assert_eq!(
            read_file_range(&store, &file_cid, "", 3, 12).unwrap(),
            b"fix linked"
        );
    }

    #[test]
    fn resolves_single_level_hamt_shard() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"hamt index";
        let file_cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&file_cid, data).unwrap();

        let hamt_data = pb_hamt(vec![link("ABindex.html", &file_cid)]);
        let hamt_cid = cid_from_data(CODEC_DAG_PB, &hamt_data);
        store.put_block(&hamt_cid, &hamt_data).unwrap();

        assert_eq!(read_file(&store, &hamt_cid, "index.html").unwrap(), data);
    }

    #[test]
    fn resolves_nested_hamt_shard() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let data = b"nested hamt";
        let file_cid = cid_from_data(CODEC_RAW, data);
        store.put_block(&file_cid, data).unwrap();

        let child_data = pb_hamt(vec![link("CDnested.txt", &file_cid)]);
        let child_cid = cid_from_data(CODEC_DAG_PB, &child_data);
        store.put_block(&child_cid, &child_data).unwrap();

        let root_data = pb_hamt(vec![link("AB", &child_cid)]);
        let root_cid = cid_from_data(CODEC_DAG_PB, &root_data);
        store.put_block(&root_cid, &root_data).unwrap();

        assert_eq!(read_file(&store, &root_cid, "nested.txt").unwrap(), data);
    }

    #[test]
    fn lists_nested_hamt_shard_entries() {
        let store = SqliteBlockStore::in_memory(1024 * 1024).unwrap();
        let alpha_cid = cid_from_data(CODEC_RAW, b"alpha");
        let nested_cid = cid_from_data(CODEC_RAW, b"nested");

        let child_data = pb_hamt(vec![link("CDnested.txt", &nested_cid)]);
        let child_cid = cid_from_data(CODEC_DAG_PB, &child_data);
        store.put_block(&child_cid, &child_data).unwrap();

        let root_data = pb_hamt(vec![
            link("AB", &child_cid),
            link("EFalpha.txt", &alpha_cid),
        ]);
        let root_cid = cid_from_data(CODEC_DAG_PB, &root_data);
        store.put_block(&root_cid, &root_data).unwrap();

        let entries = list_directory(&store, &root_cid, "").unwrap();

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.cid))
                .collect::<Vec<_>>(),
            vec![("alpha.txt", alpha_cid), ("nested.txt", nested_cid)]
        );
    }
}
