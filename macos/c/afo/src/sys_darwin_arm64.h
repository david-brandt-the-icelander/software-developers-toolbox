/* macOS arm64 raw syscall backend; the BSD syscall ABI is unsupported by Apple, so the spike measures it. */
#pragma once

#define SYS_exit 1
#define SYS_read 3
#define SYS_write 4
#define SYS_open 5
#define SYS_close 6
#define SYS_getuid 24
#define SYS_sendmsg 28
#define SYS_socket 97
#define SYS_connect 98
#define SYS_getsockopt 118
#define SYS_exit_group SYS_exit

struct afo_iovec { void *base; size_t len; };
struct afo_msghdr {
	void *name; uint32_t namelen; struct afo_iovec *iov; int32_t iovlen;
	void *control; uint32_t controllen; int32_t flags;
};
struct afo_cmsghdr { uint32_t len; int32_t level; int32_t type; };
struct afo_sockaddr_un { uint8_t len; uint8_t family; char path[104]; };

#define AFO_SOL_SOCKET 0xffff
#define AFO_CMSG_HDR 12
#define AFO_SUN_PATH_MAX 104

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
static inline uint32_t sys_fill_addr(struct afo_sockaddr_un *a, size_t pathlen)
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
