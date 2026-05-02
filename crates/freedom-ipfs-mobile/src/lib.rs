use freedom_ipfs_store::SqliteBlockStore;
use std::ffi::{c_char, CStr, CString};
use std::net::SocketAddr;
use std::ptr;
use std::sync::Mutex;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

pub struct FreedomIpfsNode {
    runtime: Runtime,
    store: SqliteBlockStore,
    gateway_addr: Mutex<Option<SocketAddr>>,
    gateway_task: Mutex<Option<JoinHandle<()>>>,
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
    Box::into_raw(Box::new(FreedomIpfsNode {
        runtime,
        store,
        gateway_addr: Mutex::new(None),
        gateway_task: Mutex::new(None),
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

    let mut gateway_task = match node.gateway_task.lock() {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    if gateway_task.is_some() {
        return true;
    }

    let store = node.store.clone();
    let listener = match node.runtime.block_on(TcpListener::bind(addr)) {
        Ok(listener) => listener,
        Err(_) => return false,
    };
    let bound = match listener.local_addr() {
        Ok(bound) => bound,
        Err(_) => return false,
    };
    let task = node.runtime.spawn(async move {
        let _ = axum::serve(listener, freedom_ipfs_gateway::router(store)).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn starts_gateway_and_reports_bound_url() {
        unsafe {
            let node = freedom_ipfs_node_new_in_memory();
            assert!(!node.is_null());

            let addr = CString::new("127.0.0.1:0").unwrap();
            assert!(freedom_ipfs_node_start_gateway(node, addr.as_ptr()));

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

            assert!(freedom_ipfs_node_stop_gateway(node));
            assert!(freedom_ipfs_node_gateway_url(node).is_null());
            freedom_ipfs_node_free(node);
        }
    }
}
