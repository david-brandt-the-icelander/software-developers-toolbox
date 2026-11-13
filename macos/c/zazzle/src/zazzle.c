/* The invoker program: a freestanding entry point around the request logic. */
#include "entry.h"
#include "zazzle_core.h"

/* Runs the round trip for this process. */
static int zazzle_main(int argc, char **argv, char **envp)
{
	return zazzle_run(argc, argv, envp);
}
