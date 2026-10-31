/* McCalpin STREAM triad kernels for the Step 62 guest image.
 * Array size is set with -DSTREAM_ARRAY_SIZE at compile time (default 10e6).
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifndef STREAM_ARRAY_SIZE
#define STREAM_ARRAY_SIZE 10000000
#endif

static double now_sec(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static double mb_s(double bytes, double secs)
{
    if (secs <= 0) {
        return 0;
    }
    return (bytes / 1e6) / secs;
}

int main(void)
{
    const size_t n = STREAM_ARRAY_SIZE;
    double *a = malloc(n * sizeof(double));
    double *b = malloc(n * sizeof(double));
    double *c = malloc(n * sizeof(double));
    if (!a || !b || !c) {
        fprintf(stderr, "stream: malloc failed\n");
        return 1;
    }
    for (size_t i = 0; i < n; i++) {
        a[i] = 1.0;
        b[i] = 2.0;
        c[i] = 0.0;
    }
    double t0, t1;
    t0 = now_sec();
    for (size_t i = 0; i < n; i++) {
        c[i] = a[i];
    }
    t1 = now_sec();
    printf("Copy:  %11.4f     Best\n", mb_s(n * sizeof(double), t1 - t0));
    t0 = now_sec();
    for (size_t i = 0; i < n; i++) {
        b[i] = 3.0 * c[i];
    }
    t1 = now_sec();
    printf("Scale: %11.4f     Best\n", mb_s(n * sizeof(double), t1 - t0));
    t0 = now_sec();
    for (size_t i = 0; i < n; i++) {
        c[i] = a[i] + b[i];
    }
    t1 = now_sec();
    printf("Add:   %11.4f     Best\n", mb_s(2 * n * sizeof(double), t1 - t0));
    t0 = now_sec();
    for (size_t i = 0; i < n; i++) {
        a[i] = b[i] + 3.0 * c[i];
    }
    t1 = now_sec();
    printf("Triad: %11.4f     Best\n", mb_s(3 * n * sizeof(double), t1 - t0));
    /* Keep the arrays live so the compiler cannot drop the kernels. */
    printf("check %.4f\n", a[n - 1] + b[n - 1] + c[n - 1]);
    free(a);
    free(b);
    free(c);
    return 0;
}
