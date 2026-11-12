/* Baseline: a minimal libc program that writes two bytes, like a typical compiled hello. */
#include <unistd.h>

/* Writes "x\n" to stdout. */
int main(void)
{
	return write(1, "x\n", 2) == 2 ? 0 : 1;
}
