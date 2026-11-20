/* Length-prefixed CS matching ternvale-gpu3d-host decode_submit. */

#include "encode.h"

static void write_u32(uint8_t *out, uint32_t value)
{
    out[0] = (uint8_t)(value & 0xffu);
    out[1] = (uint8_t)((value >> 8) & 0xffu);
    out[2] = (uint8_t)((value >> 16) & 0xffu);
    out[3] = (uint8_t)((value >> 24) & 0xffu);
}

static size_t write_hdr(uint8_t *out, size_t cap, uint32_t opcode, uint32_t size)
{
    if (out == NULL || cap < size || size < TVVK_CS_HEADER) {
        return 0;
    }
    write_u32(out, opcode);
    write_u32(out + 4, size);
    return size;
}

size_t tvvk_encode_nop(uint8_t *out, size_t cap)
{
    return write_hdr(out, cap, TVVK_CS_NOP, TVVK_CS_HEADER);
}

size_t tvvk_encode_draw_cube(uint8_t *out, size_t cap, uint32_t width, uint32_t height, uint32_t frame)
{
    if (write_hdr(out, cap, TVVK_CS_DRAW_CUBE, TVVK_DRAW_CUBE_BYTES) == 0) {
        return 0;
    }
    write_u32(out + 8, width);
    write_u32(out + 12, height);
    write_u32(out + 16, frame);
    return TVVK_DRAW_CUBE_BYTES;
}

size_t tvvk_encode_present(uint8_t *out, size_t cap)
{
    return write_hdr(out, cap, TVVK_CS_PRESENT, TVVK_CS_HEADER);
}
