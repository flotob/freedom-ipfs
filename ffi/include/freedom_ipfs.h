#ifndef FREEDOM_IPFS_H
#define FREEDOM_IPFS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FreedomIpfsNode FreedomIpfsNode;
typedef struct FreedomIpfsBuffer {
    uint8_t *data;
    size_t len;
} FreedomIpfsBuffer;

#define FREEDOM_IPFS_ROUTING_MODE_AUTO ((uint32_t)0)
#define FREEDOM_IPFS_ROUTING_MODE_DELEGATED ((uint32_t)1)
#define FREEDOM_IPFS_ROUTING_MODE_LIGHT_DHT ((uint32_t)2)

char *freedom_ipfs_version(void);
void freedom_ipfs_string_free(char *ptr);

FreedomIpfsNode *freedom_ipfs_node_new_in_memory(void);
FreedomIpfsNode *freedom_ipfs_node_new_with_data_dir(
    const char *data_dir,
    uint64_t max_cache_bytes);
void freedom_ipfs_node_free(FreedomIpfsNode *ptr);

bool freedom_ipfs_node_import_car(FreedomIpfsNode *ptr, const uint8_t *data, size_t len);
FreedomIpfsBuffer freedom_ipfs_node_export_car(FreedomIpfsNode *ptr);
void freedom_ipfs_buffer_free(FreedomIpfsBuffer buffer);
uint64_t freedom_ipfs_node_block_count(FreedomIpfsNode *ptr);
uint64_t freedom_ipfs_node_total_bytes(FreedomIpfsNode *ptr);
bool freedom_ipfs_node_clear_cache(FreedomIpfsNode *ptr);
bool freedom_ipfs_node_trim_cache(FreedomIpfsNode *ptr, uint64_t max_bytes);
bool freedom_ipfs_node_start_gateway(FreedomIpfsNode *ptr, const char *addr);
bool freedom_ipfs_node_start_gateway_online(
    FreedomIpfsNode *ptr,
    const char *addr,
    const char *delegated_router);
bool freedom_ipfs_node_start_gateway_online_with_config(
    FreedomIpfsNode *ptr,
    const char *addr,
    const char *delegated_router,
    uint32_t routing_mode,
    size_t max_concurrent_requests);
bool freedom_ipfs_node_start_gateway_online_with_config_v2(
    FreedomIpfsNode *ptr,
    const char *addr,
    const char *delegated_router,
    uint32_t routing_mode,
    size_t max_concurrent_requests,
    uint64_t dht_query_timeout_secs,
    size_t dht_max_providers);
char *freedom_ipfs_node_gateway_url(FreedomIpfsNode *ptr);
uint64_t freedom_ipfs_node_preload_path(FreedomIpfsNode *ptr, const char *path);
bool freedom_ipfs_node_cancel_preload(FreedomIpfsNode *ptr, uint64_t task_id);
bool freedom_ipfs_node_stop_gateway(FreedomIpfsNode *ptr);

#ifdef __cplusplus
}
#endif

#endif
