/* Static x86_64 probe for Linux Rosetta (Step 96). */
#include <unistd.h>

int main(void)
{
    const char msg[] = "x86_hello\n";
    if (write(1, msg, sizeof(msg) - 1) != (ssize_t)(sizeof(msg) - 1)) {
        return 1;
    }
    return 0;
}
