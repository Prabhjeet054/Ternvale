/* Guest-side virtio-fs security probes: LOOKUP/RENAME of `..` and symlink follow. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

static void rc(const char *tag, int n)
{
    printf("%s rc=%d errno=%d\n", tag, n, n < 0 ? errno : 0);
    fflush(stdout);
}

int main(int argc, char **argv)
{
    const char *mp = argc > 1 ? argv[1] : "/mnt/ternvale";
    int dir = open(mp, O_RDONLY | O_DIRECTORY);
    if (dir < 0) {
        perror("open mount");
        return 1;
    }
    rc("OPEN_DOTDOT", openat(dir, "..", O_RDONLY | O_DIRECTORY));
    int created = openat(dir, "sec_a", O_CREAT | O_RDWR | O_EXCL, 0644);
    rc("CREATE_SEC_A", created);
    if (created >= 0) {
        close(created);
    }
    rc("RENAME_DOTDOT", renameat(dir, "sec_a", dir, ".."));
    rc("SYMLINK_PASSWD", symlinkat("/etc/passwd", dir, "lp"));
    int lp = openat(dir, "lp", O_RDONLY);
    rc("OPEN_PASSWD_LINK", lp);
    if (lp >= 0) {
        char buf[80];
        ssize_t n = read(lp, buf, sizeof buf);
        printf("PASSWD_READ n=%zd\n", n);
        fflush(stdout);
        close(lp);
    }
    rc("SYMLINK_OUT", symlinkat("../outside_secret", dir, "lout"));
    int lo = openat(dir, "lout", O_RDONLY);
    rc("OPEN_OUT_LINK", lo);
    if (lo >= 0) {
        close(lo);
    }
    close(dir);
    return 0;
}
