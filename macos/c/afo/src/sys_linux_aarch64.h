/* Linux aarch64 raw syscall backend. */
#pragma once

#define SYS_read 63
#define SYS_write 64
#define SYS_close 57
#define SYS_socket 198
#define SYS_connect 203
#define SYS_sendmsg 211
#define SYS_getsockopt 209
#define SYS_getuid 174
#define SYS_openat 56
#define SYS_exit_group 94
#define SYS_clone 220
#define SYS_execve 221
#define SYS_setsid 157
#define SYS_nanosleep 101
#define SYS_readlinkat 78
#define SYS_dup3 24
#define AFO_O_DIRECTORY 0x4000

/* Issues one syscall with up to five arguments; errors come back as -errno. */
static inline long sys_call(long n, long a, long b, long c, long d, long e)
{
	register long x8 __asm__("x8") = n;
	register long x0 __asm__("x0") = a;
	register long x1 __asm__("x1") = b;
	register long x2 __asm__("x2") = c;
	register long x3 __asm__("x3") = d;
	register long x4 __asm__("x4") = e;
	__asm__ volatile("svc #0"
			 : "+r"(x0)
			 : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4)
			 : "memory");
	return x0;
}

#include "sys_linux.h"
