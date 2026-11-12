/* Startup-cost floor: the same freestanding entry as afo that exits at once. */
#include "../src/entry.h"

/* Does nothing and succeeds. */
static int afo_main(int argc, char **argv, char **envp)
{
	(void)argc;
	(void)argv;
	(void)envp;
	return 0;
}
