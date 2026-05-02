#ifndef FREEDOM_IPFS_H
#define FREEDOM_IPFS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FreedomIpfsNode FreedomIpfsNode;

char *freedom_ipfs_version(void);
void freedom_ipfs_string_free(char *ptr);

FreedomIpfsNode *freedom_ipfs_node_new_in_memory(void);
void freedom_ipfs_node_free(FreedomIpfsNode *ptr);

bool freedom_ipfs_node_import_car(FreedomIpfsNode *ptr, const uint8_t *data, size_t len);
bool freedom_ipfs_node_start_gateway(FreedomIpfsNode *ptr, const char *addr);
char *freedom_ipfs_node_gateway_url(FreedomIpfsNode *ptr);
bool freedom_ipfs_node_stop_gateway(FreedomIpfsNode *ptr);

#ifdef __cplusplus
}
#endif

#endif
