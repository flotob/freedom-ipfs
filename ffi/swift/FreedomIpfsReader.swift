import Foundation
import FreedomIpfs

public enum FreedomIpfsReaderError: Error, Equatable {
    case createNodeFailed
    case invalidNode
    case startGatewayFailed
    case importCarFailed
    case exportCarFailed
}

public enum FreedomIpfsRoutingMode: UInt32, Sendable {
    case auto = 0
    case delegated = 1
    case lightDht = 2
}

public struct FreedomIpfsStats: Equatable, Sendable {
    public let blockCount: UInt64
    public let totalBytes: UInt64
}

public final class FreedomIpfsReader {
    private var handle: OpaquePointer?

    public init() throws {
        guard let handle = freedom_ipfs_node_new_in_memory() else {
            throw FreedomIpfsReaderError.createNodeFailed
        }
        self.handle = handle
    }

    public init(dataDirectory: URL, maxCacheBytes: UInt64 = 0) throws {
        let handle = dataDirectory.path.withCString { path in
            freedom_ipfs_node_new_with_data_dir(path, maxCacheBytes)
        }
        guard let handle else {
            throw FreedomIpfsReaderError.createNodeFailed
        }
        self.handle = handle
    }

    deinit {
        if let handle {
            freedom_ipfs_node_free(handle)
        }
    }

    public static var version: String {
        guard let ptr = freedom_ipfs_version() else {
            return ""
        }
        defer { freedom_ipfs_string_free(ptr) }
        return String(cString: ptr)
    }

    public func startGateway(address: String = "127.0.0.1:0") throws {
        guard let handle else {
            throw FreedomIpfsReaderError.invalidNode
        }
        let ok = address.withCString { addressPtr in
            freedom_ipfs_node_start_gateway(handle, addressPtr)
        }
        guard ok else {
            throw FreedomIpfsReaderError.startGatewayFailed
        }
    }

    public func startOnlineGateway(
        address: String = "127.0.0.1:0",
        delegatedRouter: String? = nil,
        routingMode: FreedomIpfsRoutingMode = .auto,
        maxConcurrentRequests: Int = 0,
        dhtQueryTimeoutSeconds: UInt64 = 0,
        dhtMaxProviders: Int = 0
    ) throws {
        guard let handle else {
            throw FreedomIpfsReaderError.invalidNode
        }
        let ok = address.withCString { addressPtr in
            if let delegatedRouter {
                return delegatedRouter.withCString { routerPtr in
                    freedom_ipfs_node_start_gateway_online_with_config_v2(
                        handle,
                        addressPtr,
                        routerPtr,
                        routingMode.rawValue,
                        maxConcurrentRequests,
                        dhtQueryTimeoutSeconds,
                        dhtMaxProviders
                    )
                }
            }
            return freedom_ipfs_node_start_gateway_online_with_config_v2(
                handle,
                addressPtr,
                nil,
                routingMode.rawValue,
                maxConcurrentRequests,
                dhtQueryTimeoutSeconds,
                dhtMaxProviders
            )
        }
        guard ok else {
            throw FreedomIpfsReaderError.startGatewayFailed
        }
    }

    public var gatewayURL: URL? {
        guard let handle, let ptr = freedom_ipfs_node_gateway_url(handle) else {
            return nil
        }
        defer { freedom_ipfs_string_free(ptr) }
        return URL(string: String(cString: ptr))
    }

    public func preload(path: String) -> UInt64 {
        guard let handle else {
            return 0
        }
        return path.withCString { pathPtr in
            freedom_ipfs_node_preload_path(handle, pathPtr)
        }
    }

    public func cancelPreload(taskID: UInt64) -> Bool {
        guard let handle else {
            return false
        }
        return freedom_ipfs_node_cancel_preload(handle, taskID)
    }

    @discardableResult
    public func stopGateway() -> Bool {
        guard let handle else {
            return false
        }
        return freedom_ipfs_node_stop_gateway(handle)
    }

    public var stats: FreedomIpfsStats {
        guard let handle else {
            return FreedomIpfsStats(blockCount: 0, totalBytes: 0)
        }
        return FreedomIpfsStats(
            blockCount: freedom_ipfs_node_block_count(handle),
            totalBytes: freedom_ipfs_node_total_bytes(handle)
        )
    }

    public func clearCache() -> Bool {
        guard let handle else {
            return false
        }
        return freedom_ipfs_node_clear_cache(handle)
    }

    public func trimCache(maxBytes: UInt64) -> Bool {
        guard let handle else {
            return false
        }
        return freedom_ipfs_node_trim_cache(handle, maxBytes)
    }

    public func enterBackground() -> Bool {
        guard let handle else {
            return false
        }
        return freedom_ipfs_node_enter_background(handle)
    }

    public func enterForeground() -> Bool {
        guard let handle else {
            return false
        }
        return freedom_ipfs_node_enter_foreground(handle)
    }

    public func handleLowMemory(maxCacheBytes: UInt64 = 0) -> Bool {
        guard let handle else {
            return false
        }
        return freedom_ipfs_node_handle_low_memory(handle, maxCacheBytes)
    }

    public func handleNetworkChange() -> Bool {
        guard let handle else {
            return false
        }
        return freedom_ipfs_node_handle_network_change(handle)
    }

    public func importCar(_ data: Data) throws {
        guard let handle else {
            throw FreedomIpfsReaderError.invalidNode
        }
        let ok = data.withUnsafeBytes { bytes in
            guard let base = bytes.bindMemory(to: UInt8.self).baseAddress else {
                return false
            }
            return freedom_ipfs_node_import_car(handle, base, bytes.count)
        }
        guard ok else {
            throw FreedomIpfsReaderError.importCarFailed
        }
    }

    public func exportCar() throws -> Data {
        guard let handle else {
            throw FreedomIpfsReaderError.invalidNode
        }
        let buffer = freedom_ipfs_node_export_car(handle)
        defer { freedom_ipfs_buffer_free(buffer) }
        guard let data = buffer.data else {
            return Data()
        }
        guard buffer.len > 0 else {
            return Data()
        }
        return Data(bytes: data, count: buffer.len)
    }
}
