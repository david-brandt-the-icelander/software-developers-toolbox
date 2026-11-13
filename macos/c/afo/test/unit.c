/* Unit tests for the invoker logic, run on the host against a mock syscall layer (built with -DAFO_TEST). */
#include <stdio.h>
#include <stdlib.h>
#include "../src/afo_core.h"

static int failures, checks;

#define CHECK(cond)                                                                     \
	do {                                                                            \
		checks++;                                                               \
		if (!(cond)) {                                                          \
			failures++;                                                     \
			printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);          \
		}                                                                       \
	} while (0)

/* Resets the mock and the core's static buffers to a working default. */
static void reset(void)
{
	memset(&M, 0, sizeof M);
	memset(req, 0, sizeof req);
	memset(sockpath, 0, sizeof sockpath);
	memset(daemon_path, 0, sizeof daemon_path);
	memset(exe_path, 0, sizeof exe_path);
	memset(&addr, 0, sizeof addr);
	memset(&msg, 0, sizeof msg);
	memset(&iov, 0, sizeof iov);
	memset(&ctl, 0, sizeof ctl);
	M.socket_rc = 5;
	M.connect_n = 1;
	M.uid = 1000;
	M.peer_uid = 1000;
	M.cwd_rc = 9;
	M.read_rc = 1;
	M.recv_rc = -SYS_EAGAIN;
	M.status = 0;
	M.self_exe_rc = (long)strlen("/opt/bin/afo");
	strcpy(M.self_exe, "/opt/bin/afo");
}

/* Runs afo_run and returns its status, or -1000 minus the exit code if it exited early. */
static int run(int argc, char **argv, char **envp)
{
	if (setjmp(M.jb) == 0)
		return afo_run(argc, argv, envp);
	return -1000 - M.exit_code;
}

/* Returns true if the captured stderr contains the text. */
static int err_has(const char *s)
{
	M.err[M.err_len] = 0;
	return strstr(M.err, s) != 0;
}

/* The environment used by most tests. */
static char *env_home[] = {"PATH=/bin", "HOME=/home/u", 0};

/* Checks the happy path: request layout, descriptors, status and socket path. */
static void test_happy_path(void)
{
	reset();
	M.status = 7;
	char *argv[] = {"afo", "find", "x", 0};
	char *env[] = {"AFO_SESSION=sess1", "HOME=/home/u", 0};
	CHECK(run(3, argv, env) == 7);
	CHECK(strcmp(M.addr_path, "/home/u/.agent-file-operations/sock") == 0);
	uint32_t len;
	memcpy(&len, M.sent, 4);
	CHECK(len == M.sent_len - 4);
	CHECK(memcmp(M.sent + 4, "\0sess1\0find\0x\0", 14) == 0);
	CHECK(M.sent_fds[0] == 0 && M.sent_fds[1] == 1 && M.sent_fds[2] == 2 && M.sent_fds[3] == 9);
	CHECK(M.spawn_calls == 0);
}

/* Checks that no session leaves the field empty and no arguments still works. */
static void test_no_session_no_args(void)
{
	reset();
	char *argv[] = {"afo", 0};
	CHECK(run(1, argv, env_home) == 0);
	CHECK(M.sent_len == 4 + 2 && memcmp(M.sent + 4, "\0\0", 2) == 0);
}

/* Checks AFO_SOCK overrides HOME and lookalike variables are ignored. */
static void test_afo_sock_override(void)
{
	reset();
	char *argv[] = {"afo", 0};
	char *env[] = {"AFO_SOCKX=/no", "AFO_SOC=/no", "AFO_SOCK", "AFO_SOCK=/tmp/s", "HOME=/h", 0};
	CHECK(run(1, argv, env) == 0);
	CHECK(strcmp(M.addr_path, "/tmp/s") == 0);
}

/* Checks the path errors: no HOME, too long AFO_SOCK and too long HOME. */
static void test_path_errors(void)
{
	char *argv[] = {"afo", 0};
	char big[300];
	memset(big, 'a', sizeof big);
	big[299] = 0;
	char sock_env[320] = "AFO_SOCK=";
	strcat(sock_env, big);
	char home_env[320] = "HOME=/";
	strncat(home_env, big, 100);
	char *none[] = {"PATH=/bin", 0};
	char *long_sock[] = {sock_env, 0};
	char *long_home[] = {home_env, 0};
	reset();
	CHECK(run(1, argv, none) == -1000 - 78 && err_has("HOME is not set"));
	reset();
	CHECK(run(1, argv, long_sock) == -1000 - 78 && err_has("socket path too long"));
	reset();
	CHECK(run(1, argv, long_home) == -1000 - 78 && err_has("socket path too long"));
}

/* Checks an oversize argument list is refused. */
static void test_arguments_too_long(void)
{
	reset();
	static char huge[REQ_MAX + 10];
	memset(huge, 'x', sizeof huge - 1);
	char *argv[] = {"afo", huge, 0};
	CHECK(run(2, argv, env_home) == -1000 - 64 && err_has("arguments too long"));
}

/* Checks socket, cwd and peer failures. */
static void test_basic_failures(void)
{
	char *argv[] = {"afo", 0};
	reset();
	M.socket_rc = -24;
	CHECK(run(1, argv, env_home) == -1000 - 69 && err_has("socket failed"));
	reset();
	M.connect_rc[0] = -13;
	CHECK(run(1, argv, env_home) == -1000 - 69 && err_has("cannot connect to daemon"));
	reset();
	M.peer_uid = 0;
	CHECK(run(1, argv, env_home) == -1000 - 77 && err_has("another user"));
	reset();
	M.cwd_rc = -2;
	CHECK(run(1, argv, env_home) == -1000 - 66 && err_has("cannot open cwd"));
}

/* Checks autostart for both not-running errnos, with the daemon found beside the executable. */
static void test_autostart_paths(void)
{
	char *argv[] = {"afo", 0};
	long codes[] = {-SYS_ENOENT, -SYS_ECONNREFUSED};
	for (int i = 0; i < 2; i++) {
		reset();
		M.connect_n = 3;
		M.connect_rc[0] = codes[i];
		M.connect_rc[1] = codes[i];
		M.connect_rc[2] = 0;
		CHECK(run(1, argv, env_home) == 0);
		CHECK(M.spawn_calls == 1 && strcmp(M.spawn_path, "/opt/bin/afod") == 0);
		CHECK(M.slept_ms == 2 + 4 && M.closed == 2);
	}
}

/* Checks AFO_DAEMON overrides the daemon path and an overlong one is refused. */
static void test_afo_daemon_override(void)
{
	char *argv[] = {"afo", 0};
	reset();
	M.connect_n = 2;
	M.connect_rc[0] = -SYS_ENOENT;
	char *env[] = {"AFO_DAEMON=/usr/local/bin/afod", "HOME=/h", 0};
	CHECK(run(1, argv, env) == 0 && strcmp(M.spawn_path, "/usr/local/bin/afod") == 0);
	reset();
	M.connect_rc[0] = -SYS_ENOENT;
	char big[600];
	strcpy(big, "AFO_DAEMON=");
	memset(big + 11, 'd', 580);
	big[591] = 0;
	char *env2[] = {big, "HOME=/h", 0};
	CHECK(run(1, argv, env2) == -1000 - 78 && err_has("AFO_DAEMON too long"));
}

/* Checks the failures around finding and starting the daemon. */
static void test_autostart_failures(void)
{
	char *argv[] = {"afo", 0};
	reset();
	M.connect_rc[0] = -SYS_ENOENT;
	M.self_exe_rc = -2;
	CHECK(run(1, argv, env_home) == -1000 - 69 && err_has("daemon not running"));
	reset();
	M.connect_rc[0] = -SYS_ENOENT;
	M.self_exe_rc = 0;
	CHECK(run(1, argv, env_home) == -1000 - 69);
	reset();
	M.connect_rc[0] = -SYS_ENOENT;
	M.spawn_rc = -12;
	CHECK(run(1, argv, env_home) == -1000 - 69 && err_has("cannot start daemon (error 12)"));
	reset();
	M.connect_rc[0] = -SYS_ENOENT;
	M.spawn_rc = -100;
	CHECK(run(1, argv, env_home) == -1000 - 69 && err_has("(error 100)"));
	reset();
	M.connect_rc[0] = -SYS_ENOENT;
	memset(M.self_exe, 'e', 511);
	M.self_exe[0] = '/';
	M.self_exe[509] = '/';
	M.self_exe[511] = 0;
	M.self_exe_rc = 511;
	CHECK(run(1, argv, env_home) == -1000 - 78 && err_has("daemon path too long"));
}

/* Checks the daemon path when the executable path has no slash. */
static void test_exe_without_slash(void)
{
	char *argv[] = {"afo", 0};
	reset();
	M.connect_n = 2;
	M.connect_rc[0] = -SYS_ENOENT;
	strcpy(M.self_exe, "afo");
	M.self_exe_rc = 3;
	CHECK(run(1, argv, env_home) == 0 && strcmp(M.spawn_path, "afod") == 0);
}

/* Checks giving up when the daemon never listens, and other errors during the wait. */
static void test_wait_failures(void)
{
	char *argv[] = {"afo", 0};
	reset();
	M.connect_rc[0] = -SYS_ECONNREFUSED;
	CHECK(run(1, argv, env_home) == -1000 - 69 && err_has("daemon did not start"));
	CHECK(M.slept_ms >= START_WAIT_MS);
	reset();
	M.connect_n = 2;
	M.connect_rc[0] = -SYS_ENOENT;
	M.connect_rc[1] = -13;
	CHECK(run(1, argv, env_home) == -1000 - 69 && err_has("cannot connect to daemon"));
}

/* Checks partial sends are completed with writes and write failures are reported. */
static void test_send_paths(void)
{
	char *argv[] = {"afo", "find", "pattern", 0};
	reset();
	M.sendmsg_part = 5;
	CHECK(run(3, argv, env_home) == 0 && M.write_calls == 0);
	reset();
	M.sendmsg_part = 5;
	M.write_n = 2;
	M.write_rc[0] = 3;
	M.write_rc[1] = 1000;
	CHECK(run(3, argv, env_home) == 0 && M.write_calls == 2);
	reset();
	M.sendmsg_part = 5;
	M.write_n = 1;
	M.write_rc[0] = 0;
	CHECK(run(3, argv, env_home) == -1000 - 74 && err_has("send failed"));
	reset();
	M.sendmsg_rc = -32;
	CHECK(run(3, argv, env_home) == -1000 - 74);
}

/* Checks a daemon that closes without a status byte is reported. */
static void test_read_status_failure(void)
{
	char *argv[] = {"afo", 0};
	reset();
	M.read_rc = 0;
	CHECK(run(1, argv, env_home) == -1000 - 70 && err_has("closed without answering"));
}

/* Checks the spin-wait: a fast reply skips the blocking read, a slow one falls back to it after SPIN_US, and errors stop spinning. */
static void test_spin_wait(void)
{
	char *argv[] = {"afo", 0};
	reset();
	M.recv_rc = 1;
	M.status = 3;
	CHECK(run(1, argv, env_home) == 3 && M.recv_calls == 1 && M.read_calls == 0);
	reset();
	M.status = 4;
	CHECK(run(1, argv, env_home) == 4 && M.recv_calls == SPIN_US && M.relax_calls == SPIN_US && M.read_calls == 1);
	reset();
	M.recv_rc = 0;
	M.read_rc = 0;
	CHECK(run(1, argv, env_home) == -1000 - 70 && M.recv_calls == 1 && M.read_calls == 1);
	reset();
	M.recv_rc = -104;
	M.status = 5;
	CHECK(run(1, argv, env_home) == 5 && M.recv_calls == 1 && M.relax_calls == 0 && M.read_calls == 1);
}

/* Runs every test and reports. */
int main(void)
{
	test_happy_path();
	test_no_session_no_args();
	test_afo_sock_override();
	test_path_errors();
	test_arguments_too_long();
	test_basic_failures();
	test_autostart_paths();
	test_afo_daemon_override();
	test_autostart_failures();
	test_exe_without_slash();
	test_wait_failures();
	test_send_paths();
	test_read_status_failure();
	test_spin_wait();
	printf("%d checks, %d failures\n", checks, failures);
	return failures != 0;
}
