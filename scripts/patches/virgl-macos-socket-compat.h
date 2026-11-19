/* Linux socket flags missing on Darwin. Used only when compiling virglrenderer. */
#ifndef TERNVALE_VIRGL_MACOS_SOCKET_COMPAT_H
#define TERNVALE_VIRGL_MACOS_SOCKET_COMPAT_H
#ifndef SOCK_CLOEXEC
#define SOCK_CLOEXEC 0
#endif
#ifndef MSG_CMSG_CLOEXEC
#define MSG_CMSG_CLOEXEC 0
#endif
#endif
