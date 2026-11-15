// Allocation wrappers (malloc, calloc, realloc, free). See also memory_operations.h for the byte-level functions.
#ifndef LZFSE_ZAZZCAT_MEMORY_ALLOCATION_H
#define LZFSE_ZAZZCAT_MEMORY_ALLOCATION_H

#include <stddef.h>
#include <stdlib.h>

// Plain allocation, with no counting hook. Tests, tools, benchmarks and fuzz targets use these.
static inline void *raw_malloc(size_t size) {
    return malloc(size);
}

// Allocate `count` zeroed items of `size` bytes.
static inline void *raw_calloc(size_t count, size_t size) {
    return calloc(count, size);
}

// Resize an allocation (the old pointer stays valid when this returns NULL).
static inline void *raw_realloc(void *pointer, size_t size) {
    return realloc(pointer, size);
}

// Release an allocation.
static inline void raw_free(void *pointer) {
    free(pointer);
}

// The library's own allocation. The allocation tests build the library with LZFSE_ZAZZCAT_COUNT_ALLOCATIONS,
// which routes every library allocation through counting functions they define; normal builds use the raw ones.
#ifdef LZFSE_ZAZZCAT_COUNT_ALLOCATIONS
void *counted_malloc(size_t size);
void *counted_calloc(size_t count, size_t size);
void *counted_realloc(void *pointer, size_t size);
#define zz_malloc  counted_malloc
#define zz_calloc  counted_calloc
#define zz_realloc counted_realloc
#else
#define zz_malloc  raw_malloc
#define zz_calloc  raw_calloc
#define zz_realloc raw_realloc
#endif
#define zz_free raw_free

#endif
