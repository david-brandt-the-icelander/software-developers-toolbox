/* Linux x86_64 raw syscall backend. */
#pragma once

#define SYS_read 0
#define SYS_write 1
#define SYS_close 3
#define SYS_socket 41
#define SYS_connect 42
#define SYS_sendmsg 46
#define SYS_getsockopt 55
#define SYS_getuid 102
#define SYS_openat 257
#define SYS_exit_group 231
#define SYS_clone 56
#define SYS_execve 59
#define SYS_setsid 112
#define SYS_nanosleep 35
#define SYS_readlinkat 267
#define SYS_dup3 292
#define ZAZZLE_O_DIRECTORY 0x10000

/* Issues one syscall with up to five arguments; errors come back as -errno. */
static inline long sys_call(long n, long a, long b, long c, long d, long e)
{
	register long r10 __asm__("r10") = d;
	register long r8 __asm__("r8") = e;
	long r;
	__asm__ volatile("syscall"
			 : "=a"(r)
			 : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8)
			 : "rcx", "r11", "memory");
	return r;
}

#include "sys_linux.h"
