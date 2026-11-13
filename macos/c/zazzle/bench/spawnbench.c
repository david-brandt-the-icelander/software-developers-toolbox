/* Spawns a command repeatedly with stdout on a pipe and prints latency percentiles in microseconds. */
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>
#include <sys/wait.h>

extern char **environ;

/* Orders doubles ascending for qsort. */
static int cmp(const void *a, const void *b)
{
	double x = *(const double *)a, y = *(const double *)b;
	return (x > y) - (x < y);
}

/* Returns monotonic time in microseconds. */
static double now_us(void)
{
	struct timespec t;
	clock_gettime(CLOCK_MONOTONIC, &t);
	return (double)t.tv_sec * 1e6 + (double)t.tv_nsec / 1e3;
}

/* Spawns the command once with stdout on a pipe, drains it and returns the elapsed microseconds or -1. */
static double run_once(char **cmd)
{
	int p[2];
	if (pipe(p) != 0)
		return -1;
	posix_spawn_file_actions_t fa;
	posix_spawn_file_actions_init(&fa);
	posix_spawn_file_actions_adddup2(&fa, p[1], 1);
	posix_spawn_file_actions_addclose(&fa, p[0]);
	posix_spawn_file_actions_addclose(&fa, p[1]);
	double t0 = now_us();
	pid_t pid;
	int rc = posix_spawn(&pid, cmd[0], &fa, NULL, cmd, environ);
	close(p[1]);
	int st = 0;
	if (rc == 0)
		waitpid(pid, &st, 0);
	double dt = now_us() - t0;
	char buf[8192];
	while (read(p[0], buf, sizeof buf) > 0)
		;
	close(p[0]);
	posix_spawn_file_actions_destroy(&fa);
	return rc == 0 && WIFEXITED(st) && WEXITSTATUS(st) == 0 ? dt : -1;
}

/* Prints one result row: label, count, min, p50, p90, p99, mean. */
int main(int argc, char **argv)
{
	if (argc < 4) {
		fprintf(stderr, "usage: spawnbench N label cmd [args...]\n");
		return 2;
	}
	int n = atoi(argv[1]);
	char **cmd = argv + 3;
	for (int i = 0; i < 200; i++)
		run_once(cmd);
	double *v = malloc(sizeof(double) * (size_t)n);
	double sum = 0;
	for (int i = 0; i < n; i++) {
		v[i] = run_once(cmd);
		if (v[i] < 0) {
			fprintf(stderr, "spawnbench: %s failed\n", argv[2]);
			return 1;
		}
		sum += v[i];
	}
	qsort(v, (size_t)n, sizeof(double), cmp);
	printf("%-18s n=%d min=%.0f p50=%.0f p90=%.0f p99=%.0f mean=%.0f us\n", argv[2], n, v[0], v[n / 2],
	       v[n * 9 / 10], v[n * 99 / 100], sum / n);
	return 0;
}
