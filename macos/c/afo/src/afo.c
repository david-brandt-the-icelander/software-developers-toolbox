/* The invoker program: a freestanding entry point around the request logic. */
#include "entry.h"
#include "afo_core.h"

/* Runs the round trip for this process. */
static int afo_main(int argc, char **argv, char **envp)
{
	return afo_run(argc, argv, envp);
}
