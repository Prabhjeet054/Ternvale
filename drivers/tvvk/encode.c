/* Length-prefixed CS matching ternvale-gpu3d-host decode_submit. */

#include "encode.h"

size_t tvvk_encode_nop(uint8_t *out, size_t cap)
{
    uint32_t opcode;
    uint32_t size;
    if (out == NULL || cap < TVVK_NOP_BYTES) {
        return 0;
    }
    opcode = TVVK_CS_NOP;
    size = TVVK_CS_HEADER;
    out[0] = (uint8_t)(opcode & 0xffu);
    out[1] = (uint8_t)((opcode >> 8) & 0xffu);
    out[2] = (uint8_t)((opcode >> 16) & 0xffu);
    out[3] = (uint8_t)((opcode >> 24) & 0xffu);
    out[4] = (uint8_t)(size & 0xffu);
    out[5] = (uint8_t)((size >> 8) & 0xffu);
    out[6] = (uint8_t)((size >> 16) & 0xffu);
    out[7] = (uint8_t)((size >> 24) & 0xffu);
    return TVVK_NOP_BYTES;
}
