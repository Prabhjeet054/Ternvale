/* Write "Hello from Ternvale\n" to the UART at 0x0900_0000, then hvc #0. */
.section .text
.global _start
_start:
    movz    x1, #0
    movk    x1, #0x0900, lsl #16
    adr     x2, message
    mov     w3, #message_end - message
1:
    ldrb    w0, [x2], #1
    strb    w0, [x1]
    subs    w3, w3, #1
    b.ne    1b
    hvc     #0
message:
    .ascii "Hello from Ternvale\n"
message_end:
