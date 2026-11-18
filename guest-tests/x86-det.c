/* Deterministic x86_64 probe: identical stdout required on Rosetta, FEX,
 * and qemu-user (Step 99). Static musl. */
#include <stdint.h>
#include <stdio.h>
#include <time.h>

#ifndef DET_ITERS
#define DET_ITERS 100000u
#endif

int main(void)
{
    struct timespec a, b;
    uint32_t sum = 0;
    uint32_t i;
    long ns;

    if (clock_gettime(CLOCK_MONOTONIC, &a) != 0) {
        return 2;
    }
    for (i = 0; i < DET_ITERS; i++) {
        sum = sum * 1664525u + 1013904223u + i;
    }
    if (clock_gettime(CLOCK_MONOTONIC, &b) != 0) {
        return 3;
    }
    ns = (b.tv_sec - a.tv_sec) * 1000000000L + (b.tv_nsec - a.tv_nsec);
    /* Exact line: scripts/x86_suite/report.py checks this string. */
    printf("det v1 iters=%u sum=%u\n", DET_ITERS, sum);
    printf("det_ns=%ld\n", ns);
    return 0;
}
