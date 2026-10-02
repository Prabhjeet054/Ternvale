/*
 * Guest test for virtio-vsock: connect to the host (CID 2) on a port, send
 * "ping", and require "pong" back. Built static for aarch64 musl by
 * scripts/make-devices-initramfs.sh.
 *
 *   vsock-ping [port]      (default 5000)
 *
 * Exit codes: 0 pong received, 1 socket, 2 connect, 3 write, 4 read/EOF,
 * 5 wrong reply.
 */
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>
/* Needs struct sockaddr and sa_family_t from <sys/socket.h> first. */
#include <linux/vm_sockets.h>

static void print_local_cid(void) {
    unsigned int cid = 0;
    int dev = open("/dev/vsock", O_RDONLY);
    if (dev < 0) {
        perror("vsock-ping: open /dev/vsock");
        return;
    }
    if (ioctl(dev, IOCTL_VM_SOCKETS_GET_LOCAL_CID, &cid) == 0) {
        printf("vsock-ping: local cid %u\n", cid);
    } else {
        perror("vsock-ping: IOCTL_VM_SOCKETS_GET_LOCAL_CID");
    }
    close(dev);
}

int main(int argc, char **argv) {
    unsigned int port = argc > 1 ? (unsigned int)strtoul(argv[1], NULL, 10) : 5000;
    print_local_cid();

    int fd = socket(AF_VSOCK, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("vsock-ping: socket");
        return 1;
    }
    struct sockaddr_vm addr;
    memset(&addr, 0, sizeof addr);
    addr.svm_family = AF_VSOCK;
    addr.svm_cid = VMADDR_CID_HOST;
    addr.svm_port = port;
    if (connect(fd, (struct sockaddr *)&addr, sizeof addr) != 0) {
        perror("vsock-ping: connect");
        return 2;
    }
    printf("vsock-ping: connected to cid 2 port %u\n", port);
    fflush(stdout);

    if (write(fd, "ping", 4) != 4) {
        perror("vsock-ping: write");
        return 3;
    }
    char buf[8] = {0};
    size_t got = 0;
    while (got < 4) {
        ssize_t n = read(fd, buf + got, 4 - got);
        if (n < 0) {
            perror("vsock-ping: read");
            return 4;
        }
        if (n == 0) {
            fprintf(stderr, "vsock-ping: EOF after %zu bytes\n", got);
            return 4;
        }
        got += (size_t)n;
    }
    printf("vsock-ping: received %s\n", buf);
    close(fd);
    return strcmp(buf, "pong") == 0 ? 0 : 5;
}
