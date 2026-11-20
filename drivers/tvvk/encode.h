/* Prototype SUBMIT_3D encoder. Independent of Mesa/Yttrium. */
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opcode 1: nop. TODO(verify): not a Mesa Venus opcode. */
#define TVVK_CS_NOP 1u
/* Distinct from Venus: 'TVK' << 8 | op. Host MoltenVK executes these. */
#define TVVK_CS_DRAW_CUBE 0x54564B02u
#define TVVK_CS_PRESENT 0x54564B03u
#define TVVK_CS_HEADER 8u
#define TVVK_NOP_BYTES 8u
#define TVVK_DRAW_CUBE_BYTES 20u

size_t tvvk_encode_nop(uint8_t *out, size_t cap);
size_t tvvk_encode_draw_cube(
    uint8_t *out,
    size_t cap,
    uint32_t width,
    uint32_t height,
    uint32_t frame);
size_t tvvk_encode_present(uint8_t *out, size_t cap);

#ifdef __cplusplus
}
#endif
