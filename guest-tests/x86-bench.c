/* Tight integer loop for x86 vs ARM vs Rosetta timing (Step 98). */
#include <stdio.h>
#include <time.h>

#ifndef BENCH_ITERS
#define BENCH_ITERS 80000000UL
#endif

int main(void)
{
    struct timespec a, b;
    volatile unsigned long sum = 0;
    unsigned long i;
    long ns;

    if (clock_gettime(CLOCK_MONOTONIC, &a) != 0) {
        return 2;
    }
    for (i = 0; i < BENCH_ITERS; i++) {
        sum += i;
    }
    if (clock_gettime(CLOCK_MONOTONIC, &b) != 0) {
        return 3;
    }
    ns = (b.tv_sec - a.tv_sec) * 1000000000L + (b.tv_nsec - a.tv_nsec);
    printf("bench_ns=%ld iters=%lu sum=%lu\n", ns, BENCH_ITERS, (unsigned long)sum);
    return 0;
}
