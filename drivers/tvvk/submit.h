/* Submit tvvk CS through \\.\TVGPU3D. Windows-only. */
#pragma once

#include <stddef.h>
#include <stdint.h>

int tvvk_kernel_submit(const uint8_t *cs, size_t len);
void *tvvk_kernel_handle(void);
void tvvk_kernel_close(void);
