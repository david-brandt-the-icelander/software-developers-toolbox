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
#define SYS_recvmsg 47
#define SYS_clock_gettime 228
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

/* Returns CLOCK_MONOTONIC in nanoseconds; x86 has no portable way to learn the TSC rate from user space. */
static inline uint64_t sys_clock_now(void)
{
	struct zazzle_timespec ts;
	sys_call(SYS_clock_gettime, 1, (long)&ts, 0, 0, 0);
	return (uint64_t)ts.sec * 1000000000u + (uint64_t)ts.nsec;
}

/* Returns the clock value us microseconds from now. */
static inline uint64_t sys_clock_after_us(uint64_t us) { return sys_clock_now() + us * 1000; }

/* Tells the core this is a spin-wait loop. */
static inline void sys_cpu_relax(void) { __asm__ volatile("pause"); }
