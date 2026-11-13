/* Test double for the syscall layer: scripted results and recorded effects. */
#pragma once
#include <setjmp.h>
#include <string.h>

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
#define SYS_ENOENT 2
#define SYS_ECONNREFUSED 111
#define SYS_EAGAIN 11
#define MOCK_SENT_MAX 4096

struct mock {
	long socket_rc;
	long connect_rc[16];
	int connect_n, connect_calls;
	long peer_uid, uid, cwd_rc;
	long sendmsg_rc, sendmsg_part;
	long write_rc[8];
	int write_n, write_calls;
	long read_rc;
	int read_calls;
	long recv_rc;
	int recv_calls, relax_calls;
	uint64_t clock;
	unsigned char status;
	long self_exe_rc;
	char self_exe[512];
	long spawn_rc;
	int spawn_calls;
	char spawn_path[512];
	long slept_ms;
	int closed, exit_code;
	char err[512];
	size_t err_len;
	char sent[MOCK_SENT_MAX];
	size_t sent_len;
	int sent_fds[4];
	char addr_path[108];
	jmp_buf jb;
};
static struct mock M;

/* Records the exit code and unwinds to the test. */
static inline __attribute__((noreturn)) void sys_exit(int code)
{
	M.exit_code = code;
	longjmp(M.jb, 1);
}

/* Returns the scripted read result and status byte. */
static inline long sys_read(int fd, void *buf, size_t n)
{
	(void)fd;
	(void)n;
	M.read_calls++;
	if (M.read_rc == 1)
		*(unsigned char *)buf = M.status;
	return M.read_rc;
}

/* Returns the scripted non-blocking receive result, with the status byte when it is 1. */
static inline long sys_recv_nowait(int fd, unsigned char *b)
{
	(void)fd;
	M.recv_calls++;
	if (M.recv_rc == 1)
		*b = M.status;
	return M.recv_rc;
}

/* Returns a fake clock that advances one microsecond per reading. */
static inline uint64_t sys_clock_now(void) { return M.clock++; }

/* Returns the fake clock value us microseconds from now. */
static inline uint64_t sys_clock_after_us(uint64_t us) { return sys_clock_now() + us; }

/* Counts spin hints. */
static inline void sys_cpu_relax(void) { M.relax_calls++; }

/* Records stderr text; scripted results apply to other descriptors. */
static inline long sys_write(int fd, const void *buf, size_t n)
{
	if (fd == 2) {
		memcpy(M.err + M.err_len, buf, n);
		M.err_len += n;
		return (long)n;
	}
	if (M.write_calls < M.write_n)
		return M.write_rc[M.write_calls++];
	return (long)n;
}

/* Counts closed descriptors. */
static inline long sys_close(int fd)
{
	(void)fd;
	M.closed++;
	return 0;
}

/* Returns the scripted socket result. */
static inline long sys_socket_unix(void) { return M.socket_rc; }

/* Returns the next scripted connect result, repeating the last one, and records the path. */
static inline long sys_connect(int fd, const void *addr, uint32_t len)
{
	(void)fd;
	(void)len;
	memcpy(M.addr_path, ((const struct zazzle_sockaddr_un *)addr)->path, sizeof M.addr_path);
	int i = M.connect_calls < M.connect_n ? M.connect_calls : M.connect_n - 1;
	M.connect_calls++;
	return M.connect_rc[i];
}

/* Records the sent bytes and descriptors and returns the scripted result. */
static inline long sys_sendmsg(int fd, const struct zazzle_msghdr *m)
{
	(void)fd;
	size_t n = m->iov->len < MOCK_SENT_MAX ? m->iov->len : MOCK_SENT_MAX;
	memcpy(M.sent, m->iov->base, n);
	M.sent_len = m->iov->len;
	memcpy(M.sent_fds, (const char *)m->control + ZAZZLE_CMSG_HDR, sizeof M.sent_fds);
	if (M.sendmsg_rc < 0)
		return M.sendmsg_rc;
	return M.sendmsg_part > 0 && (size_t)M.sendmsg_part < m->iov->len ? M.sendmsg_part : (long)m->iov->len;
}

/* Returns the scripted uid. */
static inline long sys_getuid(void) { return M.uid; }

/* Returns the scripted peer uid. */
static inline long sys_peer_uid(int fd)
{
	(void)fd;
	return M.peer_uid;
}

/* Returns the scripted cwd descriptor result. */
static inline long sys_open_cwd(void) { return M.cwd_rc; }

/* Fills the address like the Linux backend. */
static inline uint32_t sys_fill_addr(struct zazzle_sockaddr_un *a, size_t pathlen)
{
	a->family = 1;
	return (uint32_t)(2 + pathlen + 1);
}

/* Returns the scripted executable path. */
static inline long sys_self_exe(char *buf, size_t cap)
{
	if (M.self_exe_rc > 0 && (size_t)M.self_exe_rc < cap)
		memcpy(buf, M.self_exe, (size_t)M.self_exe_rc + 1);
	return M.self_exe_rc;
}

/* Accumulates the time slept without sleeping. */
static inline void sys_sleep_ms(long ms) { M.slept_ms += ms; }

/* Records the daemon path and returns the scripted result. */
static inline long sys_spawn_daemon(const char *path, char **envp)
{
	(void)envp;
	M.spawn_calls++;
	strncpy(M.spawn_path, path, sizeof M.spawn_path - 1);
	return M.spawn_rc;
}
