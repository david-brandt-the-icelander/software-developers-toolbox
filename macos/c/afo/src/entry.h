/* Process entry point: raw _start without libc on Linux, or main() on macOS where libSystem is mandatory. */
#pragma once
#include "sys.h"

static int afo_main(int argc, char **argv, char **envp);

#ifdef AFO_LIBSYSTEM
int main(int argc, char **argv, char **envp) { sys_exit(afo_main(argc, argv, envp)); }
#else
__attribute__((used, noreturn)) void afo_start_c(long *sp)
{
	int argc = (int)sp[0];
	char **argv = (char **)(sp + 1);
	sys_exit(afo_main(argc, argv, argv + argc + 1));
}

#if defined(__x86_64__)
__asm__(".text\n.globl _start\n_start:\n\txor %ebp, %ebp\n\tmov %rsp, %rdi\n\tand $-16, %rsp\n\tcall afo_start_c\n");
#else
__asm__(".text\n.globl _start\n_start:\n\tmov x0, sp\n\tbl afo_start_c\n");
#endif
#endif
