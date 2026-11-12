/* Picks the syscall backend for this OS and architecture, or the test mock, and adds the thin wrappers afo uses. */
#pragma once
#include <stddef.h>
#include <stdint.h>

#if defined(AFO_TEST)
#include "../test/sys_mock.h"
#else
#if defined(__linux__) && defined(__x86_64__)
#include "sys_linux_x86_64.h"
#elif defined(__linux__) && defined(__aarch64__)
#include "sys_linux_aarch64.h"
#elif defined(__APPLE__) && defined(__aarch64__)
#include "sys_darwin_arm64.h"
#else
#error "afo supports Linux x86_64/aarch64 and macOS arm64 only"
#endif

/* Terminates the process immediately. */
static inline __attribute__((noreturn)) void sys_exit(int code)
{
	for (;;)
		sys_call(SYS_exit_group, code, 0, 0, 0, 0);
}

/* Reads up to n bytes from fd. */
static inline long sys_read(int fd, void *buf, size_t n) { return sys_call(SYS_read, fd, (long)buf, (long)n, 0, 0); }

/* Writes up to n bytes to fd. */
static inline long sys_write(int fd, const void *buf, size_t n) { return sys_call(SYS_write, fd, (long)buf, (long)n, 0, 0); }

/* Closes a descriptor. */
static inline long sys_close(int fd) { return sys_call(SYS_close, fd, 0, 0, 0, 0); }

/* Creates a unix stream socket. */
static inline long sys_socket_unix(void) { return sys_call(SYS_socket, 1, 1, 0, 0, 0); }

/* Connects a socket to an address. */
static inline long sys_connect(int fd, const void *addr, uint32_t len) { return sys_call(SYS_connect, fd, (long)addr, len, 0, 0); }

/* Sends a message with optional ancillary data. */
static inline long sys_sendmsg(int fd, const struct afo_msghdr *m) { return sys_call(SYS_sendmsg, fd, (long)m, 0, 0, 0); }

/* Returns the real uid of this process. */
static inline long sys_getuid(void) { return sys_call(SYS_getuid, 0, 0, 0, 0, 0); }
#endif
