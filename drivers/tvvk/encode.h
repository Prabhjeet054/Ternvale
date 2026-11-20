/* Prototype SUBMIT_3D encoder. Independent of Mesa/Yttrium. */
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opcode 1: nop, 8-byte header, no object ids. TODO(verify): not Mesa Venus. */
#define TVVK_CS_NOP 1u
#define TVVK_CS_HEADER 8u

#define TVVK_NOP_BYTES 8u

/* Write an 8-byte nop CS into `out` (must hold TVVK_NOP_BYTES). Returns length. */
size_t tvvk_encode_nop(uint8_t *out, size_t cap);

#ifdef __cplusplus
}
#endif
