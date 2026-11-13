/* arm64 spin helpers: the generic timer counter, readable from user space on Linux and macOS, and the yield hint. */
#pragma once

/* Returns the generic timer's virtual count. */
static inline uint64_t sys_clock_now(void)
{
	uint64_t t;
	__asm__ volatile("mrs %0, cntvct_el0" : "=r"(t));
	return t;
}

/* Returns the counter value us microseconds from now, using the timer frequency. */
static inline uint64_t sys_clock_after_us(uint64_t us)
{
	uint64_t f;
	__asm__ volatile("mrs %0, cntfrq_el0" : "=r"(f));
	return sys_clock_now() + f * us / 1000000;
}

/* Tells the core this is a spin-wait loop. */
static inline void sys_cpu_relax(void) { __asm__ volatile("yield"); }
