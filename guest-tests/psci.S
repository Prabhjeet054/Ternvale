/* Call PSCI_VERSION, print x0 as eight hex digits on the UART, then SYSTEM_OFF. */
.section .text
.global _start
_start:
    movz    x0, #0
    movk    x0, #0x8400, lsl #16
    hvc     #0
    mov     w5, w0
    movz    x1, #0
    movk    x1, #0x0900, lsl #16
    mov     w3, #8
1:
    lsr     w4, w5, #28
    and     w4, w4, #0xf
    cmp     w4, #10
    b.lo    2f
    add     w4, w4, #87
    b       3f
2:
    add     w4, w4, #48
3:
    strb    w4, [x1]
    lsl     w5, w5, #4
    subs    w3, w3, #1
    b.ne    1b
    mov     w4, #10
    strb    w4, [x1]
    movz    x0, #8
    movk    x0, #0x8400, lsl #16
    hvc     #0
4:
    b       4b
