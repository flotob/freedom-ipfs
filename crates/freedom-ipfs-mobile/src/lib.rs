use freedom_ipfs_namesys::{
    CachedNameResolver, CloudflareDohResolver, DefaultNameResolver, DelegatedIpnsResolver,
    FallbackIpnsResolver, IpnsResolver,
};
use freedom_ipfs_retrieval::FetchingBlockProvider;
use freedom_ipfs_routing::{
    AutoRoutingClient, DelegatedRoutingClient, DhtIpnsResolver, LightDhtClient,
    ProviderRoutingClient, DEFAULT_DELEGATED_ROUTER,
};
use freedom_ipfs_store::SqliteBlockStore;
use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

const DEFAULT_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const CACHE_DB_FILE: &str = "freedom-ipfs.sqlite3";
const ROUTING_MODE_AUTO: u32 = 0;
const ROUTING_MODE_DELEGATED: u32 = 1;
const ROUTING_MODE_LIGHT_DHT: u32 = 2;
const PRELOAD_TIMEOUT: Duration = Duration::from_secs(60);

pub struct FreedomIpfsNode {
    runtime: Runtime,
    store: SqliteBlockStore,
    gateway_addr: Mutex<Option<SocketAddr>>,
    gateway_task: Mutex<Option<JoinHandle<()>>>,
    next_preload_id: AtomicU64,
    preload_tasks: Mutex<HashMap<u64, JoinHandle<()>>>,
}

#[repr(C)]
pub struct FreedomIpfsBuffer {
    pub data: *mut u8,
    pub len: usize,
}

#[no_mangle]
pub extern "C" fn freedom_ipfs_version() -> *mut c_char {
    CString::new(env!("CARGO_PKG_VERSION"))
        .expect("version has no nul")
        .into_raw()
}

/// # Safety
///
/// `ptr` must be a pointer returned by `freedom_ipfs_version` and must not be
/// freed more than once.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_string_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        let _ = CString::from_raw(ptr);
    }
}

#[no_mangle]
pub extern "C" fn freedom_ipfs_node_new_in_memory() -> *mut FreedomIpfsNode {
    let store = match SqliteBlockStore::in_memory(DEFAULT_CACHE_BYTES) {
        Ok(store) => store,
        Err(_) => return ptr::null_mut(),
    };
    node_from_store(store)
}

/// # Safety
///
/// `data_dir` must point to a NUL-terminated UTF-8 path string for the
/// duration of this call. `max_cache_bytes` may be 0 to use the default 256 MiB
/// cache budget.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_new_with_data_dir(
    data_dir: *const c_char,
    max_cache_bytes: u64,
) -> *mut FreedomIpfsNode {
    if data_dir.is_null() {
        return ptr::null_mut();
    }
    let data_dir = match CStr::from_ptr(data_dir).to_str() {
        Ok(path) => PathBuf::from(path),
        Err(_) => return ptr::null_mut(),
    };
    if fs::create_dir_all(&data_dir).is_err() {
        return ptr::null_mut();
    }
    let max_cache_bytes = if max_cache_bytes == 0 {
        DEFAULT_CACHE_BYTES
    } else {
        max_cache_bytes
    };
    let store = match SqliteBlockStore::open(data_dir.join(CACHE_DB_FILE), max_cache_bytes) {
        Ok(store) => store,
        Err(_) => return ptr::null_mut(),
    };
    node_from_store(store)
}

fn node_from_store(store: SqliteBlockStore) -> *mut FreedomIpfsNode {
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(_) => return ptr::null_mut(),
    };
    Box::into_raw(Box::new(FreedomIpfsNode {
        runtime,
        store,
        gateway_addr: Mutex::new(None),
        gateway_task: Mutex::new(None),
        next_preload_id: AtomicU64::new(1),
        preload_tasks: Mutex::new(HashMap::new()),
    }))
}

/// # Safety
///
/// `ptr` must be a pointer returned by `freedom_ipfs_node_new_in_memory` and
/// must not be used after this function returns.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_free(ptr: *mut FreedomIpfsNode) {
    if !ptr.is_null() {
        let node = &*ptr;
        stop_gateway(node);
        stop_preloads(node);
        let _ = Box::from_raw(ptr);
    }
}

/// # Safety
///
/// `ptr` must be a valid node pointer. `data` must point to `len` readable
/// bytes for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_import_car(
    ptr: *mut FreedomIpfsNode,
    data: *const u8,
    len: usize,
) -> bool {
    if ptr.is_null() || data.is_null() {
        return false;
    }
    let node = &*ptr;
    let bytes = std::slice::from_raw_parts(data, len);
    node.store.import_car(bytes).is_ok()
}

/// # Safety
///
/// `ptr` must be a valid node pointer. The returned buffer must be released
/// with `freedom_ipfs_buffer_free`.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_export_car(
    ptr: *mut FreedomIpfsNode,
) -> FreedomIpfsBuffer {
    if ptr.is_null() {
        return empty_buffer();
    }
    let node = &*ptr;
    match node.store.export_car() {
        Ok(bytes) => buffer_from_vec(bytes),
        Err(_) => empty_buffer(),
    }
}

/// # Safety
///
/// `buffer` must be a buffer returned by `freedom_ipfs_node_export_car` and
/// must not be freed more than once.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_buffer_free(buffer: FreedomIpfsBuffer) {
    if buffer.data.is_null() {
        return;
    }
    let slice = std::ptr::slice_from_raw_parts_mut(buffer.data, buffer.len);
    let _ = Box::from_raw(slice);
}

fn buffer_from_vec(bytes: Vec<u8>) -> FreedomIpfsBuffer {
    if bytes.is_empty() {
        return empty_buffer();
    }
    let mut bytes = bytes.into_boxed_slice();
    let data = bytes.as_mut_ptr();
    let len = bytes.len();
    let _ = Box::into_raw(bytes);
    FreedomIpfsBuffer { data, len }
}

fn empty_buffer() -> FreedomIpfsBuffer {
    FreedomIpfsBuffer {
        data: ptr::null_mut(),
        len: 0,
    }
}

/// # Safety
///
/// `ptr` must be a valid node pointer.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_block_count(ptr: *mut FreedomIpfsNode) -> u64 {
    if ptr.is_null() {
        return 0;
    }
    let node = &*ptr;
    node.store.block_count().unwrap_or(0)
}

/// # Safety
///
/// `ptr` must be a valid node pointer.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_total_bytes(ptr: *mut FreedomIpfsNode) -> u64 {
    if ptr.is_null() {
        return 0;
    }
    let node = &*ptr;
    node.store.total_bytes().unwrap_or(0)
}

/// # Safety
///
/// `ptr` must be a valid node pointer.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_clear_cache(ptr: *mut FreedomIpfsNode) -> bool {
    if ptr.is_null() {
        return false;
    }
    let node = &*ptr;
    node.store.clear().is_ok()
}

/// # Safety
///
/// `ptr` must be a valid node pointer.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_trim_cache(
    ptr: *mut FreedomIpfsNode,
    max_bytes: u64,
) -> bool {
    if ptr.is_null() {
        return false;
    }
    let node = &*ptr;
    node.store.trim_blocks_to(max_bytes).is_ok()
}

/// # Safety
///
/// `ptr` must be a valid node pointer. `addr` must point to a NUL-terminated
/// UTF-8 socket address string for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_start_gateway(
    ptr: *mut FreedomIpfsNode,
    addr: *const c_char,
) -> bool {
    if ptr.is_null() || addr.is_null() {
        return false;
    }
    let node = &*ptr;
    let addr = match CStr::from_ptr(addr)
        .to_str()
        .ok()
        .and_then(|s| s.parse::<SocketAddr>().ok())
    {
        Some(addr) => addr,
        None => return false,
    };

    let store = node.store.clone();
    start_gateway_with_router(node, addr, freedom_ipfs_gateway::router(store))
}

/// # Safety
///
/// `ptr` must be a valid node pointer. `addr` must point to a NUL-terminated
/// UTF-8 socket address string for the duration of this call. `delegated_router`
/// may be null to use the default delegated routing endpoint, otherwise it must
/// point to a NUL-terminated UTF-8 URL string.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_start_gateway_online(
    ptr: *mut FreedomIpfsNode,
    addr: *const c_char,
    delegated_router: *const c_char,
) -> bool {
    freedom_ipfs_node_start_gateway_online_with_config(
        ptr,
        addr,
        delegated_router,
        ROUTING_MODE_AUTO,
        0,
    )
}

/// # Safety
///
/// `ptr` must be a valid node pointer. `addr` must point to a NUL-terminated
/// UTF-8 socket address string for the duration of this call. `delegated_router`
/// may be null to use the default delegated routing endpoint, otherwise it must
/// point to a NUL-terminated UTF-8 URL string. `routing_mode` must be one of the
/// `FREEDOM_IPFS_ROUTING_MODE_*` constants from the C header.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_start_gateway_online_with_config(
    ptr: *mut FreedomIpfsNode,
    addr: *const c_char,
    delegated_router: *const c_char,
    routing_mode: u32,
    max_concurrent_requests: usize,
) -> bool {
    freedom_ipfs_node_start_gateway_online_with_config_v2(
        ptr,
        addr,
        delegated_router,
        routing_mode,
        max_concurrent_requests,
        0,
        0,
    )
}

/// # Safety
///
/// `ptr` must be a valid node pointer. `addr` must point to a NUL-terminated
/// UTF-8 socket address string for the duration of this call. `delegated_router`
/// may be null to use the default delegated routing endpoint, otherwise it must
/// point to a NUL-terminated UTF-8 URL string. `routing_mode` must be one of the
/// `FREEDOM_IPFS_ROUTING_MODE_*` constants from the C header. `dht_*` values may
/// be 0 to use the built-in mobile defaults.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_start_gateway_online_with_config_v2(
    ptr: *mut FreedomIpfsNode,
    addr: *const c_char,
    delegated_router: *const c_char,
    routing_mode: u32,
    max_concurrent_requests: usize,
    dht_query_timeout_secs: u64,
    dht_max_providers: usize,
) -> bool {
    if ptr.is_null() || addr.is_null() {
        return false;
    }
    let node = &*ptr;
    let addr = match CStr::from_ptr(addr)
        .to_str()
        .ok()
        .and_then(|s| s.parse::<SocketAddr>().ok())
    {
        Some(addr) => addr,
        None => return false,
    };
    let delegated_router = if delegated_router.is_null() {
        DEFAULT_DELEGATED_ROUTER.to_string()
    } else {
        match CStr::from_ptr(delegated_router).to_str() {
            Ok(router) => router.to_string(),
            Err(_) => return false,
        }
    };

    let delegated = DelegatedRoutingClient::new(delegated_router.clone());
    let dht = light_dht_client(dht_query_timeout_secs, dht_max_providers);
    let routing = match routing_mode {
        ROUTING_MODE_AUTO => {
            ProviderRoutingClient::from(AutoRoutingClient::new(delegated, dht.clone()))
        }
        ROUTING_MODE_DELEGATED => ProviderRoutingClient::from(delegated),
        ROUTING_MODE_LIGHT_DHT => ProviderRoutingClient::from(dht.clone()),
        _ => return false,
    };
    let provider = FetchingBlockProvider::new(node.store.clone(), routing);
    let gateway_config = if max_concurrent_requests == 0 {
        freedom_ipfs_gateway::GatewayConfig::default()
    } else {
        freedom_ipfs_gateway::GatewayConfig::new(max_concurrent_requests)
    };
    let name_resolver = CachedNameResolver::new(DefaultNameResolver::new(
        CloudflareDohResolver::default(),
        ipns_resolver(routing_mode, delegated_router, dht),
    ));
    start_gateway_with_router(
        node,
        addr,
        freedom_ipfs_gateway::router_with_provider_and_name_resolver_config(
            Arc::new(provider),
            Arc::new(name_resolver),
            gateway_config,
        ),
    )
}

fn light_dht_client(dht_query_timeout_secs: u64, dht_max_providers: usize) -> LightDhtClient {
    let mut dht = LightDhtClient::default();
    if dht_query_timeout_secs != 0 {
        dht = dht.with_query_timeout(Duration::from_secs(dht_query_timeout_secs));
    }
    if dht_max_providers != 0 {
        dht = dht.with_max_providers(dht_max_providers);
    }
    dht
}

fn ipns_resolver(
    routing_mode: u32,
    delegated_router: String,
    dht: LightDhtClient,
) -> Arc<dyn IpnsResolver> {
    match routing_mode {
        ROUTING_MODE_AUTO => Arc::new(FallbackIpnsResolver::new(
            DelegatedIpnsResolver::new(delegated_router),
            DhtIpnsResolver::new(dht),
        )),
        ROUTING_MODE_DELEGATED => Arc::new(DelegatedIpnsResolver::new(delegated_router)),
        ROUTING_MODE_LIGHT_DHT => Arc::new(DhtIpnsResolver::new(dht)),
        _ => Arc::new(DelegatedIpnsResolver::new(delegated_router)),
    }
}

fn start_gateway_with_router(
    node: &FreedomIpfsNode,
    addr: SocketAddr,
    router: axum::Router,
) -> bool {
    let mut gateway_task = match node.gateway_task.lock() {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    if gateway_task.is_some() {
        return true;
    }

    let listener = match node.runtime.block_on(TcpListener::bind(addr)) {
        Ok(listener) => listener,
        Err(_) => return false,
    };
    let bound = match listener.local_addr() {
        Ok(bound) => bound,
        Err(_) => return false,
    };
    let task = node.runtime.spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    *gateway_task = Some(task);
    if let Ok(mut gateway_addr) = node.gateway_addr.lock() {
        *gateway_addr = Some(bound);
    }
    true
}

/// # Safety
///
/// `ptr` must be a valid node pointer.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_gateway_url(ptr: *mut FreedomIpfsNode) -> *mut c_char {
    if ptr.is_null() {
        return ptr::null_mut();
    }
    let node = &*ptr;
    let Ok(gateway_addr) = node.gateway_addr.lock() else {
        return ptr::null_mut();
    };
    let Some(addr) = *gateway_addr else {
        return ptr::null_mut();
    };
    match CString::new(format!("http://{addr}")) {
        Ok(url) => url.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// # Safety
///
/// `ptr` must be a valid node pointer. `path` must point to a NUL-terminated
/// UTF-8 `/ipfs/...` or `/ipns/...` gateway path for the duration of this call.
/// Returns 0 when the gateway is not running or the path is invalid.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_preload_path(
    ptr: *mut FreedomIpfsNode,
    path: *const c_char,
) -> u64 {
    if ptr.is_null() || path.is_null() {
        return 0;
    }
    let node = &*ptr;
    let path = match CStr::from_ptr(path).to_str() {
        Ok(path) if is_preload_path(path) => path.to_string(),
        _ => return 0,
    };
    let Ok(gateway_addr) = node.gateway_addr.lock() else {
        return 0;
    };
    let Some(addr) = *gateway_addr else {
        return 0;
    };
    drop(gateway_addr);

    let id = node.next_preload_id.fetch_add(1, Ordering::Relaxed);
    let url = format!("http://{addr}{path}");
    let task = node.runtime.spawn(async move {
        let Ok(client) = reqwest::Client::builder().timeout(PRELOAD_TIMEOUT).build() else {
            return;
        };
        let Ok(response) = client.get(url).send().await else {
            return;
        };
        let Ok(mut response) = response.error_for_status() else {
            return;
        };
        while matches!(response.chunk().await, Ok(Some(_))) {}
    });

    let Ok(mut tasks) = node.preload_tasks.lock() else {
        task.abort();
        return 0;
    };
    tasks.retain(|_, task| !task.is_finished());
    tasks.insert(id, task);
    id
}

/// # Safety
///
/// `ptr` must be a valid node pointer. `task_id` must be an id returned by
/// `freedom_ipfs_node_preload_path`.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_cancel_preload(
    ptr: *mut FreedomIpfsNode,
    task_id: u64,
) -> bool {
    if ptr.is_null() || task_id == 0 {
        return false;
    }
    let node = &*ptr;
    let Ok(mut tasks) = node.preload_tasks.lock() else {
        return false;
    };
    let Some(task) = tasks.remove(&task_id) else {
        return false;
    };
    task.abort();
    true
}

/// # Safety
///
/// `ptr` must be a valid node pointer.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_stop_gateway(ptr: *mut FreedomIpfsNode) -> bool {
    if ptr.is_null() {
        return false;
    }
    let node = &*ptr;
    stop_gateway(node);
    true
}

fn stop_gateway(node: &FreedomIpfsNode) {
    if let Ok(mut gateway_task) = node.gateway_task.lock() {
        if let Some(task) = gateway_task.take() {
            task.abort();
        }
    }
    if let Ok(mut gateway_addr) = node.gateway_addr.lock() {
        *gateway_addr = None;
    }
}

fn stop_preloads(node: &FreedomIpfsNode) {
    if let Ok(mut tasks) = node.preload_tasks.lock() {
        for (_, task) in tasks.drain() {
            task.abort();
        }
    }
}

fn is_preload_path(path: &str) -> bool {
    path.starts_with("/ipfs/") || path.starts_with("/ipns/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use freedom_ipfs_core::{cid_from_data, parse_car_v1, CODEC_RAW};
    use std::io::{Read, Write};

    #[test]
    fn starts_gateway_and_reports_bound_url() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let addr = CString::new("127.0.0.1:0").unwrap();
            assert!(freedom_ipfs_node_start_gateway(node, addr.as_ptr()));

            assert_gateway_health(node);

            assert!(freedom_ipfs_node_stop_gateway(node));
            assert!(freedom_ipfs_node_gateway_url(node).is_null());
            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn starts_online_gateway_and_reports_bound_url() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let addr = CString::new("127.0.0.1:0").unwrap();
            assert!(freedom_ipfs_node_start_gateway_online(
                node,
                addr.as_ptr(),
                ptr::null(),
            ));

            assert_gateway_health(node);

            assert!(freedom_ipfs_node_stop_gateway(node));
            assert!(freedom_ipfs_node_gateway_url(node).is_null());
            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn starts_online_gateway_with_config() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let addr = CString::new("127.0.0.1:0").unwrap();
            let router = CString::new("http://127.0.0.1:9/routing/v1").unwrap();
            assert!(freedom_ipfs_node_start_gateway_online_with_config(
                node,
                addr.as_ptr(),
                router.as_ptr(),
                ROUTING_MODE_DELEGATED,
                1,
            ));

            assert_gateway_health(node);

            assert!(freedom_ipfs_node_stop_gateway(node));
            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn starts_online_gateway_with_dht_budget_config() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let addr = CString::new("127.0.0.1:0").unwrap();
            let router = CString::new("http://127.0.0.1:9/routing/v1").unwrap();
            assert!(freedom_ipfs_node_start_gateway_online_with_config_v2(
                node,
                addr.as_ptr(),
                router.as_ptr(),
                ROUTING_MODE_AUTO,
                1,
                5,
                2,
            ));

            assert_gateway_health(node);

            assert!(freedom_ipfs_node_stop_gateway(node));
            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn rejects_invalid_routing_mode() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let addr = CString::new("127.0.0.1:0").unwrap();
            assert!(!freedom_ipfs_node_start_gateway_online_with_config(
                node,
                addr.as_ptr(),
                ptr::null(),
                99,
                0,
            ));
            assert!(freedom_ipfs_node_gateway_url(node).is_null());
            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn preloads_gateway_path_and_allows_cancel() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let data = b"preload me";
            let cid = cid_from_data(CODEC_RAW, data);
            (*node).store.put_block(&cid, data).unwrap();
            let addr = CString::new("127.0.0.1:0").unwrap();
            assert!(freedom_ipfs_node_start_gateway(node, addr.as_ptr()));

            let bad_path = CString::new("https://example.com/ipfs/not-local").unwrap();
            assert_eq!(freedom_ipfs_node_preload_path(node, bad_path.as_ptr()), 0);

            let path = CString::new(format!("/ipfs/{cid}")).unwrap();
            let task_id = freedom_ipfs_node_preload_path(node, path.as_ptr());
            assert!(task_id > 0);
            assert!(freedom_ipfs_node_cancel_preload(node, task_id));
            assert!(!freedom_ipfs_node_cancel_preload(node, task_id));

            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn reports_and_clears_cache_stats() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let data = b"mobile stats";
            let cid = cid_from_data(CODEC_RAW, data);
            (*node).store.put_block(&cid, data).unwrap();

            assert_eq!(freedom_ipfs_node_block_count(node), 1);
            assert_eq!(freedom_ipfs_node_total_bytes(node), data.len() as u64);
            assert!(freedom_ipfs_node_clear_cache(node));
            assert_eq!(freedom_ipfs_node_block_count(node), 0);
            assert_eq!(freedom_ipfs_node_total_bytes(node), 0);

            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn exports_cache_as_car_buffer() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let data = b"mobile export";
            let cid = cid_from_data(CODEC_RAW, data);
            (*node).store.put_block(&cid, data).unwrap();

            let buffer = freedom_ipfs_node_export_car(node);
            assert!(!buffer.data.is_null());
            assert!(buffer.len > 0);
            let bytes = std::slice::from_raw_parts(buffer.data, buffer.len);
            let car = parse_car_v1(bytes).unwrap();
            assert_eq!(car.blocks.len(), 1);
            assert_eq!(car.blocks[0].cid, cid);
            assert_eq!(car.blocks[0].data, data);
            freedom_ipfs_buffer_free(buffer);

            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn trims_cache_to_requested_budget() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let first = vec![1u8; 16];
            let second = vec![2u8; 16];
            let first_cid = cid_from_data(CODEC_RAW, &first);
            let second_cid = cid_from_data(CODEC_RAW, &second);
            (*node).store.put_block(&first_cid, &first).unwrap();
            (*node).store.put_block(&second_cid, &second).unwrap();

            assert!(freedom_ipfs_node_trim_cache(node, 20));
            assert!(freedom_ipfs_node_total_bytes(node) <= 20);
            assert_eq!(freedom_ipfs_node_block_count(node), 1);

            freedom_ipfs_node_free(node);
        }
    }

    #[test]
    fn opens_persistent_data_dir_cache() {
        unsafe {
            let tempdir = tempfile::tempdir().unwrap();
            let data_dir = CString::new(tempdir.path().to_str().unwrap()).unwrap();
            let node = freedom_ipfs_node_new_with_data_dir(data_dir.as_ptr(), 1024 * 1024);
            assert!(!node.is_null());

            let data = b"persisted mobile cache";
            let cid = cid_from_data(CODEC_RAW, data);
            (*node).store.put_block(&cid, data).unwrap();
            assert_eq!(freedom_ipfs_node_block_count(node), 1);
            freedom_ipfs_node_free(node);

            let reopened = freedom_ipfs_node_new_with_data_dir(data_dir.as_ptr(), 1024 * 1024);
            assert!(!reopened.is_null());
            assert_eq!(freedom_ipfs_node_block_count(reopened), 1);
            assert_eq!(freedom_ipfs_node_total_bytes(reopened), data.len() as u64);
            freedom_ipfs_node_free(reopened);
        }
    }

    unsafe fn assert_gateway_health(node: *mut FreedomIpfsNode) {
        let url_ptr = freedom_ipfs_node_gateway_url(node);
        assert!(!url_ptr.is_null());
        let url = CStr::from_ptr(url_ptr).to_str().unwrap().to_string();
        freedom_ipfs_string_free(url_ptr);
        assert!(url.starts_with("http://127.0.0.1:"));

        let addr = url.strip_prefix("http://").unwrap();
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("200 OK"));
        assert!(response.ends_with("ok\n"));
    }
}
