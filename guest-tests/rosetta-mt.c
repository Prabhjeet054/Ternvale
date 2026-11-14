/* Multi-threaded x86_64 probe for Linux Rosetta (Step 96). */
#include <pthread.h>
#include <stdio.h>

static volatile int n;

static void *inc(void *arg)
{
    (void)arg;
    __sync_add_and_fetch(&n, 1);
    return NULL;
}

int main(void)
{
    pthread_t t[4];
    int i;

    for (i = 0; i < 4; i++) {
        if (pthread_create(&t[i], NULL, inc, NULL) != 0) {
            return 2;
        }
    }
    for (i = 0; i < 4; i++) {
        if (pthread_join(t[i], NULL) != 0) {
            return 3;
        }
    }
    printf("mt_x86 n=%d\n", n);
    return n == 4 ? 0 : 1;
}
