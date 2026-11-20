/* Guest 3D kill switch and kernel-path probe. Fail closed → 0 Vulkan devices. */
#pragma once

#ifdef __cplusplus
extern "C" {
#endif

/* Non-zero if the ICD may advertise a physical device. */
int tvvk_gpu3d_enabled(void);

#ifdef __cplusplus
}
#endif
