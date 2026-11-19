// Byte-level memory operations (memcpy, memmove, memset, memcmp). Together with memory_allocation.h, the only
// place that calls the C library's memory functions; everything else goes through these wrappers, so memory
// handling can be audited, instrumented or replaced in one place.
#ifndef LZFSE_ZAZZCAT_MEMORY_OPERATIONS_H
#define LZFSE_ZAZZCAT_MEMORY_OPERATIONS_H

#include <stdbool.h>
#include <stddef.h>
#include <string.h>

// Copy `count` bytes between regions that do not overlap. With a constant `count` this compiles to plain loads
// and stores, which is how unaligned integer access is done safely in C.
static inline void copy_bytes(void *destination, const void *source, size_t count) {
    memcpy(destination, source, count);
}

// Copy `count` bytes between regions that may overlap.
static inline void move_bytes(void *destination, const void *source, size_t count) {
    memmove(destination, source, count);
}

// Set `count` bytes to `value`.
static inline void fill_bytes(void *destination, int value, size_t count) {
    memset(destination, value, count);
}

// Set `count` bytes to zero.
static inline void clear_bytes(void *destination, size_t count) {
    memset(destination, 0, count);
}

// Compare `count` bytes; returns 0 when equal, otherwise the sign of the first difference.
static inline int compare_bytes(const void *a, const void *b, size_t count) {
    // not used in the real code, just tests, tools, benchmarks
    return memcmp(a, b, count);
}

// True when the first `count` bytes of `a` and `b` are the same. Use this for equality checks and keep
// compare_bytes for callers that need the order.
static inline bool equal_bytes(const void *a, const void *b, size_t count) {
    // not used in the real code, just tests, tools, benchmarks
    return compare_bytes(a, b, count) == 0;
}

#endif
