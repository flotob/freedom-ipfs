use freedom_ipfs_store::SqliteBlockStore;
use std::ffi::{c_char, CStr, CString};
use std::net::SocketAddr;
use std::ptr;
use tokio::runtime::Runtime;

pub struct FreedomIpfsNode {
    runtime: Runtime,
    store: SqliteBlockStore,
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
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(_) => return ptr::null_mut(),
    };
    let store = match SqliteBlockStore::in_memory(256 * 1024 * 1024) {
        Ok(store) => store,
        Err(_) => return ptr::null_mut(),
    };
    Box::into_raw(Box::new(FreedomIpfsNode { runtime, store }))
}

/// # Safety
///
/// `ptr` must be a pointer returned by `freedom_ipfs_node_new_in_memory` and
/// must not be used after this function returns.
#[no_mangle]
pub unsafe extern "C" fn freedom_ipfs_node_free(ptr: *mut FreedomIpfsNode) {
    if !ptr.is_null() {
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
    node.runtime.spawn(async move {
        let _ = freedom_ipfs_gateway::serve(store, addr).await;
    });
    true
}
