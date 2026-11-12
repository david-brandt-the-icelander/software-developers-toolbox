/* Phase 0 invoker: sends argv and fds 0,1,2 plus a cwd fd to the daemon and exits with its status byte. */
#include "entry.h"

#define REQ_MAX 65536
#define NFDS 4

static char req[REQ_MAX + 4];
static char resp[REQ_MAX];
static char sockpath[AFO_SUN_PATH_MAX];
static struct afo_sockaddr_un addr;
static struct afo_msghdr msg;
static struct afo_iovec iov;
static union { struct afo_cmsghdr h; char raw[64]; } ctl;

/* Returns the length of s. */
static size_t str_len(const char *s)
{
	size_t n = 0;
	while (s[n])
		n++;
	return n;
}

/* Prints a one-line error to stderr and exits with code. */
static __attribute__((noreturn)) void die(const char *m, int code)
{
	sys_write(2, "afo: ", 5);
	sys_write(2, m, str_len(m));
	sys_write(2, "\n", 1);
	sys_exit(code);
}

/* Returns the value of NAME in envp, or 0. */
static const char *env_get(char **envp, const char *name)
{
	size_t n = str_len(name);
	for (; *envp; envp++) {
		size_t i = 0;
		while (i < n && (*envp)[i] == name[i])
			i++;
		if (i == n && (*envp)[n] == '=')
			return *envp + n + 1;
	}
	return 0;
}

/* Appends s to sockpath at offset *at, failing if it does not fit. */
static void path_append(size_t *at, const char *s)
{
	size_t n = str_len(s);
	if (*at + n + 1 > AFO_SUN_PATH_MAX)
		die("socket path too long", 78);
	for (size_t i = 0; i < n; i++)
		sockpath[*at + i] = s[i];
	*at += n;
}

/* Builds the socket path from AFO_SOCK or $HOME/.agent-file-operations/sock. */
static size_t build_path(char **envp)
{
	size_t at = 0;
	const char *s = env_get(envp, "AFO_SOCK");
	if (s) {
		path_append(&at, s);
		return at;
	}
	s = env_get(envp, "HOME");
	if (!s)
		die("HOME is not set", 78);
	path_append(&at, s);
	path_append(&at, "/.agent-file-operations/sock");
	return at;
}

/* Copies argv[1..] NUL-separated behind a 4-byte length header and returns the total size. */
static size_t build_request(int argc, char **argv)
{
	size_t at = 4;
	for (int i = 1; i < argc; i++) {
		size_t n = str_len(argv[i]) + 1;
		if (at + n > REQ_MAX + 4)
			die("arguments too long", 64);
		for (size_t j = 0; j < n; j++)
			req[at + j] = argv[i][j];
		at += n;
	}
	*(uint32_t *)req = (uint32_t)(at - 4);
	return at;
}

/* Connects to the daemon and checks that the peer runs as our uid. */
static int connect_daemon(char **envp)
{
	size_t n = build_path(envp);
	long fd = sys_socket_unix();
	if (fd < 0)
		die("socket failed", 69);
	for (size_t i = 0; i < n; i++)
		addr.path[i] = sockpath[i];
	uint32_t len = sys_fill_addr(&addr, n);
	if (sys_connect((int)fd, &addr, len) < 0)
		die("daemon not running", 69);
	if (sys_peer_uid((int)fd) != sys_getuid())
		die("daemon runs as another user", 77);
	return (int)fd;
}

/* Sends the whole request, attaching the fds on the first message unless relaying. */
static void send_request(int fd, size_t total)
{
	iov.base = req;
	iov.len = total;
	msg.iov = &iov;
	msg.iovlen = 1;
#ifdef AFO_RELAY
	(void)ctl;
#else
	long cwd = sys_open_cwd();
	if (cwd < 0)
		die("cannot open cwd", 66);
	int *fds = (int *)(ctl.raw + AFO_CMSG_HDR);
	ctl.h.len = AFO_CMSG_HDR + NFDS * 4;
	ctl.h.level = AFO_SOL_SOCKET;
	ctl.h.type = 1;
	fds[0] = 0;
	fds[1] = 1;
	fds[2] = 2;
	fds[3] = (int)cwd;
	msg.control = &ctl;
	msg.controllen = ctl.h.len;
#endif
	long sent = sys_sendmsg(fd, &msg);
	if (sent < 0)
		die("send failed", 74);
	while ((size_t)sent < total) {
		long w = sys_write(fd, req + sent, total - (size_t)sent);
		if (w <= 0)
			die("send failed", 74);
		sent += w;
	}
}

/* Reads the daemon's answer and returns the exit status it ends with. */
static int read_status(int fd)
{
#ifdef AFO_RELAY
	size_t have = 0;
	for (;;) {
		long r = sys_read(fd, resp + have, sizeof resp - have);
		if (r < 0)
			die("read failed", 74);
		if (r == 0)
			break;
		have += (size_t)r;
	}
	if (have == 0)
		die("daemon closed without answering", 70);
	sys_write(1, resp, have - 1);
	return (unsigned char)resp[have - 1];
#else
	unsigned char b;
	if (sys_read(fd, &b, 1) != 1)
		die("daemon closed without answering", 70);
	(void)resp;
	return b;
#endif
}

/* Runs one request/answer round trip. */
static int afo_main(int argc, char **argv, char **envp)
{
	int fd = connect_daemon(envp);
	send_request(fd, build_request(argc, argv));
	return read_status(fd);
}
