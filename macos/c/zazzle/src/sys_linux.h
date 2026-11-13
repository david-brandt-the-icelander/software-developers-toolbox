/* Linux structure layouts and constants shared by the x86_64 and aarch64 backends. */
#pragma once

struct zazzle_iovec { void *base; size_t len; };
struct zazzle_msghdr {
	void *name; uint32_t namelen; struct zazzle_iovec *iov; size_t iovlen;
	void *control; size_t controllen; int32_t flags;
};
struct zazzle_cmsghdr { size_t len; int32_t level; int32_t type; };
struct zazzle_sockaddr_un { uint16_t family; char path[108]; };

#define ZAZZLE_SOL_SOCKET 1
#define ZAZZLE_CMSG_HDR 16
#define ZAZZLE_SUN_PATH_MAX 108

/* Fills the socket address and returns its length. */
static inline uint32_t sys_fill_addr(struct zazzle_sockaddr_un *a, size_t pathlen)
{
	a->family = 1;
	return (uint32_t)(2 + pathlen + 1);
}

/* Opens the current directory and returns the fd or a negative errno. */
static inline long sys_open_cwd(void)
{
	return sys_call(SYS_openat, -100, (long)".", ZAZZLE_O_DIRECTORY, 0, 0);
}

/* Returns the uid of the process on the other end of the socket, or -1. */
static inline long sys_peer_uid(int fd)
{
	uint32_t cred[3];
	uint32_t len = sizeof cred;
	long r = sys_call(SYS_getsockopt, fd, ZAZZLE_SOL_SOCKET, 17, (long)cred, (long)&len);
	return r < 0 ? -1 : (long)cred[1];
}

#define SYS_ENOENT 2
#define SYS_ECONNREFUSED 111

struct zazzle_timespec { long sec; long nsec; };

/* Writes this executable's path into buf and returns its length, or a negative errno. */
static inline long sys_self_exe(char *buf, size_t cap)
{
	long n = sys_call(SYS_readlinkat, -100, (long)"/proc/self/exe", (long)buf, (long)cap - 1, 0);
	if (n > 0)
		buf[n] = 0;
	return n;
}

/* Sleeps for the given number of milliseconds. */
static inline void sys_sleep_ms(long ms)
{
	struct zazzle_timespec ts;
	ts.sec = ms / 1000;
	ts.nsec = (ms % 1000) * 1000000;
	sys_call(SYS_nanosleep, (long)&ts, 0, 0, 0, 0);
}

/* Starts "path serve" detached from this process with standard streams on /dev/null; returns the child pid or a negative errno. */
static inline long sys_spawn_daemon(const char *path, char **envp)
{
	long pid = sys_call(SYS_clone, 17, 0, 0, 0, 0);
	if (pid != 0)
		return pid;
	char *argv[3];
	argv[0] = (char *)path;
	argv[1] = (char *)"serve";
	argv[2] = 0;
	sys_call(SYS_setsid, 0, 0, 0, 0, 0);
	long null = sys_call(SYS_openat, -100, (long)"/dev/null", 2, 0, 0);
	for (long fd = 0; fd < 3; fd++)
		sys_call(SYS_dup3, null, fd, 0, 0, 0);
	sys_call(SYS_execve, (long)path, (long)argv, (long)envp, 0, 0);
	for (;;)
		sys_call(SYS_exit_group, 127, 0, 0, 0, 0);
}
