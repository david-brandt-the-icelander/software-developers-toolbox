/* macOS arm64 raw syscall backend; the BSD syscall ABI is unsupported by Apple, so the spike measures it. */
#pragma once

#define SYS_exit 1
#define SYS_read 3
#define SYS_write 4
#define SYS_open 5
#define SYS_close 6
#define SYS_getuid 24
#define SYS_sendmsg 28
#define SYS_recvmsg 27
#define SYS_socket 97
#define SYS_connect 98
#define SYS_getsockopt 118
#define SYS_exit_group SYS_exit

struct zazzle_iovec { void *base; size_t len; };
struct zazzle_msghdr {
	void *name; uint32_t namelen; struct zazzle_iovec *iov; int32_t iovlen;
	void *control; uint32_t controllen; int32_t flags;
};
struct zazzle_cmsghdr { uint32_t len; int32_t level; int32_t type; };
struct zazzle_sockaddr_un { uint8_t len; uint8_t family; char path[104]; };

#define ZAZZLE_SOL_SOCKET 0xffff
#define ZAZZLE_CMSG_HDR 12
#define ZAZZLE_SUN_PATH_MAX 104

/* Issues one syscall with up to five arguments; errors come back as -errno via the carry flag. */
static inline long sys_call(long n, long a, long b, long c, long d, long e)
{
	register long x16 __asm__("x16") = n;
	register long x0 __asm__("x0") = a;
	register long x1 __asm__("x1") = b;
	register long x2 __asm__("x2") = c;
	register long x3 __asm__("x3") = d;
	register long x4 __asm__("x4") = e;
	__asm__ volatile("svc #0x80\n\tb.cc 1f\n\tneg x0, x0\n1:"
			 : "+r"(x0)
			 : "r"(x16), "r"(x1), "r"(x2), "r"(x3), "r"(x4)
			 : "memory", "cc");
	return x0;
}

/* Fills the socket address and returns its length. */
static inline uint32_t sys_fill_addr(struct zazzle_sockaddr_un *a, size_t pathlen)
{
	a->len = (uint8_t)(2 + pathlen + 1);
	a->family = 1;
	return (uint32_t)(2 + pathlen + 1);
}

/* Opens the current directory and returns the fd or a negative errno. */
static inline long sys_open_cwd(void)
{
	return sys_call(SYS_open, (long)".", 0x100000, 0, 0, 0);
}

/* Returns the uid of the process on the other end of the socket, or -1. */
static inline long sys_peer_uid(int fd)
{
	uint32_t cred[19];
	uint32_t len = sizeof cred;
	long r = sys_call(SYS_getsockopt, fd, 0, 1, (long)cred, (long)&len);
	return r < 0 ? -1 : (long)cred[1];
}

#define SYS_execve 59
#define SYS_ENOENT 2
#define SYS_ECONNREFUSED 61
#define SYS_EAGAIN 35
#define ZAZZLE_MSG_DONTWAIT 0x80

#include "sys_arm64_clock.h"

/* libSystem entry points, declared by hand so no SDK headers are needed. */
extern int _NSGetExecutablePath(char *buf, uint32_t *size);
extern int nanosleep(const void *req, void *rem);
extern int posix_spawn(int *pid, const char *path, void **file_actions, void **attr, char *const argv[], char *const envp[]);
extern int posix_spawn_file_actions_init(void **fa);
extern int posix_spawn_file_actions_addopen(void **fa, int fd, const char *path, int oflag, unsigned short mode);
extern int posix_spawnattr_init(void **attr);
extern int posix_spawnattr_setflags(void **attr, short flags);

/* Writes this executable's path into buf and returns its length, or a negative errno. */
static inline long sys_self_exe(char *buf, size_t cap)
{
	uint32_t size = (uint32_t)cap;
	if (_NSGetExecutablePath(buf, &size) != 0)
		return -1;
	long n = 0;
	while (buf[n])
		n++;
	return n;
}

/* Sleeps for the given number of milliseconds. */
static inline void sys_sleep_ms(long ms)
{
	long ts[2];
	ts[0] = ms / 1000;
	ts[1] = (ms % 1000) * 1000000;
	nanosleep(ts, 0);
}

/* Starts "path serve" in its own session with standard streams on /dev/null; returns the child pid or a negative errno. */
static inline long sys_spawn_daemon(const char *path, char **envp)
{
	void *fa = 0, *attr = 0;
	char *argv[3];
	int pid = 0;
	argv[0] = (char *)path;
	argv[1] = (char *)"serve";
	argv[2] = 0;
	posix_spawn_file_actions_init(&fa);
	for (int fd = 0; fd < 3; fd++)
		posix_spawn_file_actions_addopen(&fa, fd, "/dev/null", 2, 0);
	posix_spawnattr_init(&attr);
	posix_spawnattr_setflags(&attr, 0x400); /* POSIX_SPAWN_SETSID */
	int rc = posix_spawn(&pid, path, &fa, &attr, argv, envp);
	return rc == 0 ? pid : -rc;
}

/* Replaces this process with path (argv[0] is the first element); returns a negative errno only if that fails. */
static inline long sys_exec(const char *path, char **argv, char **envp)
{
	return sys_call(SYS_execve, (long)path, (long)argv, (long)envp, 0, 0);
}
