/* x86 TSO litmus: message-passing and store-buffering without fences.
 *
 * MP (Wx=1; Wy=1 || Ry; Rx): x86 forbids (r1=1, r2=0). A hit is a TSO
 * violation in the emulator.
 * SB (Wx=1; Ry || Wy=1; Rx): x86 *allows* (0,0) via the store buffer.
 * We count both_zero; it is not a violation.
 *
 * Payload uses relaxed atomics only. Handshake flags are seq_cst so the
 * harness cannot livelock if the translator is weak.
 */
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifndef LITMUS_ITERS
#define LITMUS_ITERS 2000000UL
#endif

enum Mode { MODE_MP, MODE_SB };

static volatile uint32_t x, y;
static volatile uint32_t r0, r1;
static volatile uint32_t go;
static volatile uint32_t done0, done1;
static volatile uint32_t stop;
static enum Mode mode;

static void pause_cpu(void)
{
    static __thread unsigned spins;
#if defined(__x86_64__) || defined(__i386__)
    __asm__ volatile("pause" ::: "memory");
#endif
    spins++;
    if ((spins & 63u) == 0) {
        sched_yield();
    }
}

static void *p0(void *arg)
{
    uint32_t seen = 0;
    (void)arg;
    for (;;) {
        uint32_t g;
        while ((g = __atomic_load_n(&go, __ATOMIC_SEQ_CST)) == seen) {
            if (__atomic_load_n(&stop, __ATOMIC_SEQ_CST)) {
                return NULL;
            }
            pause_cpu();
        }
        seen = g;
        if (mode == MODE_MP) {
            __atomic_store_n(&x, 1u, __ATOMIC_RELAXED);
            __atomic_store_n(&y, 1u, __ATOMIC_RELAXED);
        } else {
            __atomic_store_n(&x, 1u, __ATOMIC_RELAXED);
            r0 = __atomic_load_n(&y, __ATOMIC_RELAXED);
        }
        __atomic_store_n(&done0, seen, __ATOMIC_SEQ_CST);
    }
}

static void *p1(void *arg)
{
    uint32_t seen = 0;
    (void)arg;
    for (;;) {
        uint32_t g;
        while ((g = __atomic_load_n(&go, __ATOMIC_SEQ_CST)) == seen) {
            if (__atomic_load_n(&stop, __ATOMIC_SEQ_CST)) {
                return NULL;
            }
            pause_cpu();
        }
        seen = g;
        if (mode == MODE_MP) {
            r1 = __atomic_load_n(&y, __ATOMIC_RELAXED);
            r0 = __atomic_load_n(&x, __ATOMIC_RELAXED);
        } else {
            __atomic_store_n(&y, 1u, __ATOMIC_RELAXED);
            r1 = __atomic_load_n(&x, __ATOMIC_RELAXED);
        }
        __atomic_store_n(&done1, seen, __ATOMIC_SEQ_CST);
    }
}

static int run(enum Mode m, unsigned long iters)
{
    pthread_t t0, t1;
    struct timespec a, b;
    unsigned long i;
    unsigned long hits = 0;
    long ns;

    mode = m;
    x = y = r0 = r1 = 0;
    go = done0 = done1 = stop = 0;
    if (pthread_create(&t0, NULL, p0, NULL) != 0) {
        return 2;
    }
    if (pthread_create(&t1, NULL, p1, NULL) != 0) {
        return 3;
    }
    if (clock_gettime(CLOCK_MONOTONIC, &a) != 0) {
        return 4;
    }
    printf("litmus %s start iters=%lu\n", m == MODE_MP ? "mp" : "sb", iters);
    fflush(stdout);
    for (i = 1; i <= iters; i++) {
        __atomic_store_n(&x, 0u, __ATOMIC_RELAXED);
        __atomic_store_n(&y, 0u, __ATOMIC_RELAXED);
        r0 = r1 = 0;
        __atomic_store_n(&go, (uint32_t)i, __ATOMIC_SEQ_CST);
        while (__atomic_load_n(&done0, __ATOMIC_SEQ_CST) != (uint32_t)i ||
               __atomic_load_n(&done1, __ATOMIC_SEQ_CST) != (uint32_t)i) {
            pause_cpu();
        }
        if (m == MODE_MP) {
            if (r1 == 1u && r0 == 0u) {
                hits++;
            }
        } else if (r0 == 0u && r1 == 0u) {
            hits++;
        }
        if ((i % 500000UL) == 0) {
            printf("litmus %s progress=%lu hits=%lu\n",
                   m == MODE_MP ? "mp" : "sb", i, hits);
            fflush(stdout);
        }
    }
    if (clock_gettime(CLOCK_MONOTONIC, &b) != 0) {
        return 5;
    }
    ns = (b.tv_sec - a.tv_sec) * 1000000000L + (b.tv_nsec - a.tv_nsec);
    __atomic_store_n(&stop, 1u, __ATOMIC_SEQ_CST);
    __atomic_store_n(&go, (uint32_t)(iters + 1), __ATOMIC_SEQ_CST);
    pthread_join(t0, NULL);
    pthread_join(t1, NULL);
    if (m == MODE_MP) {
        printf("litmus mp iters=%lu forbidden=%lu ns=%ld\n", iters, hits, ns);
        fflush(stdout);
        return hits == 0 ? 0 : 1;
    }
    printf("litmus sb iters=%lu both_zero=%lu ns=%ld\n", iters, hits, ns);
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv)
{
    const char *kind = "mp";
    unsigned long iters = LITMUS_ITERS;

    if (argc > 1) {
        kind = argv[1];
    }
    if (argc > 2) {
        iters = strtoul(argv[2], NULL, 10);
        if (iters == 0) {
            return 6;
        }
    }
    if (strcmp(kind, "mp") == 0) {
        return run(MODE_MP, iters);
    }
    if (strcmp(kind, "sb") == 0) {
        return run(MODE_SB, iters);
    }
    fprintf(stderr, "usage: x86-litmus mp|sb [iters]\n");
    return 7;
}
