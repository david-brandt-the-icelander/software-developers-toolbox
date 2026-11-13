/* The invoker's logic: builds a request from argv and the environment, sends it with the standard streams and cwd, and returns the daemon's status. */
#pragma once
#include "sys.h"

#define REQ_MAX 262144
#define NFDS 4
#define START_WAIT_MS 3000
#define EXE_MAX 512
#define SPIN_US 25

static char req[REQ_MAX + 4] __attribute__((aligned(8)));
static char sockpath[ZAZZLE_SUN_PATH_MAX];
static char daemon_path[EXE_MAX];
static char exe_path[EXE_MAX];
static struct zazzle_sockaddr_un addr;
static struct zazzle_msghdr msg;
static struct zazzle_iovec iov;
static union { struct zazzle_cmsghdr h; char raw[64]; } ctl;

/* Returns the length of s. */
static size_t str_len(const char *s)
{
	size_t n = 0;
	while (s[n])
		n++;
	return n;
}

/* Returns true if a and b are equal strings. */
static int str_eq(const char *a, const char *b)
{
	size_t i = 0;
	while (a[i] && a[i] == b[i])
		i++;
	return a[i] == b[i];
}

/* Prints a one-line error to stderr and exits with code. */
static __attribute__((noreturn)) void die(const char *m, int code)
{
	sys_write(2, "zazzle: ", 5);
	sys_write(2, m, str_len(m));
	sys_write(2, "\n", 1);
	sys_exit(code);
}

/* Prints a one-line error with a decimal error number and exits with code. */
static __attribute__((noreturn)) void die_errno(const char *m, long err, int code)
{
	char digits[24];
	size_t n = 0;
	sys_write(2, "zazzle: ", 5);
	sys_write(2, m, str_len(m));
	sys_write(2, " (error ", 8);
	do {
		digits[n++] = (char)('0' + err % 10);
		err /= 10;
	} while (err > 0);
	while (n > 0)
		sys_write(2, &digits[--n], 1);
	sys_write(2, ")\n", 2);
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

/* Appends s to buf at offset *at, failing with code if it does not fit in cap bytes including the NUL. */
static void append(char *buf, size_t cap, size_t *at, const char *s, const char *what, int code)
{
	size_t n = str_len(s);
	if (*at + n + 1 > cap)
		die(what, code);
	for (size_t i = 0; i < n; i++)
		buf[*at + i] = s[i];
	*at += n;
	buf[*at] = 0;
}

/* Builds the socket path from ZAZZLE_SOCK or $HOME/.zazzle/sock. */
static size_t build_path(char **envp)
{
	size_t at = 0;
	const char *s = env_get(envp, "ZAZZLE_SOCK");
	if (s) {
		append(sockpath, sizeof sockpath, &at, s, "socket path too long", 78);
		return at;
	}
	s = env_get(envp, "HOME");
	if (!s)
		die("HOME is not set", 78);
	append(sockpath, sizeof sockpath, &at, s, "socket path too long", 78);
	append(sockpath, sizeof sockpath, &at, "/.zazzle/sock", "socket path too long", 78);
	return at;
}

/* Appends one NUL-terminated request field at offset at and returns the new offset. */
static size_t put_field(size_t at, const char *s)
{
	size_t n = str_len(s) + 1;
	if (at + n > REQ_MAX + 4)
		die("arguments too long", 64);
	for (size_t j = 0; j < n; j++)
		req[at + j] = s[j];
	return at + n;
}

/* Builds the request: length, empty cwd (the daemon uses the passed descriptor), session, then argv[1..]. */
static size_t build_request(int argc, char **argv, char **envp)
{
	size_t at = 4;
	const char *session = env_get(envp, "ZAZZLE_SESSION");
	at = put_field(at, "");
	at = put_field(at, session ? session : "");
	for (int i = 1; i < argc; i++)
		at = put_field(at, argv[i]);
	*(uint32_t *)req = (uint32_t)(at - 4);
	return at;
}

/* Makes one connection attempt and returns the descriptor, or a negative errno from connect. */
static long try_connect(size_t pathlen)
{
	long fd = sys_socket_unix();
	if (fd < 0)
		die("socket failed", 69);
	for (size_t i = 0; i <= pathlen; i++)
		addr.path[i] = sockpath[i];
	uint32_t len = sys_fill_addr(&addr, pathlen);
	long r = sys_connect((int)fd, &addr, len);
	if (r < 0) {
		sys_close((int)fd);
		return r;
	}
	return fd;
}

/* Returns true if the errno means no daemon is listening yet. */
static int not_running(long r)
{
	return r == -SYS_ENOENT || r == -SYS_ECONNREFUSED;
}

/* Writes the directory of this executable, with a trailing slash, into exe_path; returns false if unknown. */
static int exe_dir(void)
{
	long n = sys_self_exe(exe_path, sizeof exe_path);
	if (n <= 0)
		return 0;
	while (n > 0 && exe_path[n - 1] != '/')
		n--;
	exe_path[n] = 0;
	return 1;
}

/* Chooses the daemon executable: $ZAZZLE_DAEMON, else "zazzled" beside this executable. */
static void pick_daemon(char **envp)
{
	size_t at = 0;
	const char *s = env_get(envp, "ZAZZLE_DAEMON");
	if (s) {
		append(daemon_path, sizeof daemon_path, &at, s, "ZAZZLE_DAEMON too long", 78);
		return;
	}
	if (!exe_dir())
		die("daemon not running", 69);
	append(daemon_path, sizeof daemon_path, &at, exe_path, "daemon path too long", 78);
	append(daemon_path, sizeof daemon_path, &at, "zazzled", "daemon path too long", 78);
}

/* Returns true if the argument names a standalone tool that runs as its own process, not through the daemon. */
static int is_tool(const char *a)
{
	return str_eq(a, "free-disk-space") || str_eq(a, "fix-network");
}

/* Replaces this process with the tool named by argv[1], found beside this executable, passing the rest of the arguments; returns only on failure. */
static void exec_tool(char **argv, char **envp)
{
	size_t at = 0;
	if (!exe_dir())
		die("cannot find the tool", 69);
	append(daemon_path, sizeof daemon_path, &at, exe_path, "tool path too long", 78);
	append(daemon_path, sizeof daemon_path, &at, argv[1], "tool path too long", 78);
	argv[1] = daemon_path;
	long r = sys_exec(daemon_path, argv + 1, envp);
	die_errno("cannot run the tool", -r, 69);
}

/* Connects to the daemon, starting it and retrying with backoff if it is not running. */
static int connect_daemon(size_t pathlen, char **envp)
{
	long fd = try_connect(pathlen);
	if (fd < 0 && !not_running(fd))
		die("cannot connect to daemon", 69);
	if (fd < 0) {
		pick_daemon(envp);
		long pid = sys_spawn_daemon(daemon_path, envp);
		if (pid < 0)
			die_errno("cannot start daemon", -pid, 69);
	}
	long waited = 0, step = 2;
	while (fd < 0 && waited < START_WAIT_MS) {
		sys_sleep_ms(step);
		waited += step;
		step = step < 100 ? step * 2 : step;
		fd = try_connect(pathlen);
		if (fd < 0 && !not_running(fd))
			die("cannot connect to daemon", 69);
	}
	if (fd < 0)
		die("daemon did not start", 69);
	if (sys_peer_uid((int)fd) != sys_getuid())
		die("daemon runs as another user", 77);
	return (int)fd;
}

/* Sends the whole request, attaching the standard streams and cwd descriptor on the first message. */
static void send_request(int fd, size_t total)
{
	long cwd = sys_open_cwd();
	if (cwd < 0)
		die("cannot open cwd", 66);
	iov.base = req;
	iov.len = total;
	msg.iov = &iov;
	msg.iovlen = 1;
	int *fds = (int *)(ctl.raw + ZAZZLE_CMSG_HDR);
	ctl.h.len = ZAZZLE_CMSG_HDR + NFDS * 4;
	ctl.h.level = ZAZZLE_SOL_SOCKET;
	ctl.h.type = 1;
	fds[0] = 0;
	fds[1] = 1;
	fds[2] = 2;
	fds[3] = (int)cwd;
	msg.control = &ctl;
	msg.controllen = ctl.h.len;
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

/* Reads the daemon's one-byte exit status, polling without sleeping for up to SPIN_US first so a fast reply skips the wake-up. */
static int read_status(int fd)
{
	unsigned char b;
	uint64_t end = sys_clock_after_us(SPIN_US);
	do {
		long r = sys_recv_nowait(fd, &b);
		if (r == 1)
			return b;
		if (r != -SYS_EAGAIN)
			break;
		sys_cpu_relax();
	} while (sys_clock_now() < end);
	if (sys_read(fd, &b, 1) != 1)
		die("daemon closed without answering", 70);
	return b;
}

/* Runs a tool as its own process (exec), or one request/answer round trip, and returns the daemon's exit status. */
static int zazzle_run(int argc, char **argv, char **envp)
{
	if (argc > 1 && is_tool(argv[1]))
		exec_tool(argv, envp);
	size_t total = build_request(argc, argv, envp);
	size_t pathlen = build_path(envp);
	int fd = connect_daemon(pathlen, envp);
	send_request(fd, total);
	return read_status(fd);
}
