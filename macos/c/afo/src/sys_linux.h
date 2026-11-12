/* Linux structure layouts and constants shared by the x86_64 and aarch64 backends. */
#pragma once

struct afo_iovec { void *base; size_t len; };
struct afo_msghdr {
	void *name; uint32_t namelen; struct afo_iovec *iov; size_t iovlen;
	void *control; size_t controllen; int32_t flags;
};
struct afo_cmsghdr { size_t len; int32_t level; int32_t type; };
struct afo_sockaddr_un { uint16_t family; char path[108]; };

#define AFO_SOL_SOCKET 1
#define AFO_CMSG_HDR 16
#define AFO_SUN_PATH_MAX 108

/* Fills the socket address and returns its length. */
static inline uint32_t sys_fill_addr(struct afo_sockaddr_un *a, size_t pathlen)
{
	a->family = 1;
	return (uint32_t)(2 + pathlen + 1);
}

/* Opens the current directory and returns the fd or a negative errno. */
static inline long sys_open_cwd(void)
{
	return sys_call(SYS_openat, -100, (long)".", AFO_O_DIRECTORY, 0, 0);
}

/* Returns the uid of the process on the other end of the socket, or -1. */
static inline long sys_peer_uid(int fd)
{
	uint32_t cred[3];
	uint32_t len = sizeof cred;
	long r = sys_call(SYS_getsockopt, fd, AFO_SOL_SOCKET, 17, (long)cred, (long)&len);
	return r < 0 ? -1 : (long)cred[1];
}
