#ifndef TV_VZ_H
#define TV_VZ_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* C ABI for ternvale-vz. Chosen instead of swift-bridge so the Swift
 * package stays a small static library, queue hopping is explicit, and
 * rustc does not take a procedural-macro dependency on generated glue. */

typedef void (*tv_vz_log_fn)(int32_t level, const char *message);
typedef void (*tv_vz_state_fn)(void *ctx, int32_t state, const char *detail);

void tv_vz_set_logger(tv_vz_log_fn fn);

int32_t tv_vz_create(
    const char *name,
    uint32_t cpus,
    uint64_t ram_bytes,
    const char *kernel,
    const char *initrd,
    const char *cmdline,
    const char *nvram,
    const char *serial_log,
    const char *uds_dir,
    const uint32_t *ports,
    uint32_t port_count,
    const char *disks,
    const uint8_t *disk_ro,
    uint32_t disk_count,
    uint8_t attach_vsock,
    uint8_t attach_nat,
    tv_vz_state_fn cb,
    void *ctx,
    char *err,
    size_t err_len);

int32_t tv_vz_start(int32_t handle, char *err, size_t err_len);
int32_t tv_vz_pause(int32_t handle, char *err, size_t err_len);
int32_t tv_vz_resume(int32_t handle, char *err, size_t err_len);
int32_t tv_vz_stop(int32_t handle, uint8_t force, char *err, size_t err_len);
int32_t tv_vz_state(int32_t handle);
void tv_vz_destroy(int32_t handle);

#ifdef __cplusplus
}
#endif

#endif
