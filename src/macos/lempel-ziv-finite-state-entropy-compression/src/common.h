// Shared helpers for every source file: integer types, compiler hints, little-endian loads and stores, and the
// memory wrappers.
#ifndef LZFSE_ZAZZCAT_COMMON_H
#define LZFSE_ZAZZCAT_COMMON_H

#include "memory_allocation.h"
#include "memory_operations.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

#include "lzfse_zazzcat.h"

// `static inline` functions are copied into each file that includes this header; ALWAYS_INLINE asks the compiler
// to paste the body into every caller, which hot loops rely on.
#define ALWAYS_INLINE static inline __attribute__((always_inline))
// Keep a function out of line (its callers stay small).
#define NOINLINE __attribute__((noinline))
// Mark a function as rarely called, so the compiler moves it (and the code that calls it) out of hot paths.
#define COLD __attribute__((cold, noinline))
// Calling convention for a rarely called function that a hot loop calls: the callee saves almost every register,
// so the loop keeps its state in registers across the call. Only Clang on 64-bit Arm and x86-64 supports it (GCC
// lacks it, and Clang rejects it on other targets), so elsewhere this expands to nothing.
#if defined(__clang__) && (defined(__aarch64__) || defined(__x86_64__))
#define PRESERVE_REGISTERS __attribute__((preserve_most))
#else
#define PRESERVE_REGISTERS
#endif
// Branch hints: the condition is almost always true (LIKELY) or false (UNLIKELY).
#define LIKELY(condition)   __builtin_expect(!!(condition), 1)
#define UNLIKELY(condition) __builtin_expect(!!(condition), 0)

// Smaller and larger of two sizes.
ALWAYS_INLINE size_t min_size(size_t a, size_t b) {
    return a < b ? a : b;
}

// Larger of two sizes.
ALWAYS_INLINE size_t max_size(size_t a, size_t b) {
    return a > b ? a : b;
}

// Round `value / divisor` up.
ALWAYS_INLINE size_t div_ceil(size_t value, size_t divisor) {
    return value / divisor + (value % divisor != 0);
}

// Loads and stores of unaligned little-endian integers. `memcpy` with a constant size compiles to one load or
// store instruction; reading through a cast pointer instead would be undefined behaviour in C.
ALWAYS_INLINE uint16_t load_u16(const uint8_t *at) {
    uint16_t value;
    copy_bytes(&value, at, sizeof value);
    return value;
}

// Load 4 bytes as a little-endian integer.
ALWAYS_INLINE uint32_t load_u32(const uint8_t *at) {
    uint32_t value;
    copy_bytes(&value, at, sizeof value);
    return value;
}

// Load 8 bytes as a little-endian integer.
ALWAYS_INLINE uint64_t load_u64(const uint8_t *at) {
    uint64_t value;
    copy_bytes(&value, at, sizeof value);
    return value;
}

// Store a 16-bit little-endian integer.
ALWAYS_INLINE void store_u16(uint8_t *at, uint16_t value) {
    copy_bytes(at, &value, sizeof value);
}

// Store a 32-bit little-endian integer.
ALWAYS_INLINE void store_u32(uint8_t *at, uint32_t value) {
    copy_bytes(at, &value, sizeof value);
}

// Store a 64-bit little-endian integer.
ALWAYS_INLINE void store_u64(uint8_t *at, uint64_t value) {
    copy_bytes(at, &value, sizeof value);
}

// Multi-value loads and stores for fields that sit next to each other. On a little-endian CPU an array of
// integers has exactly the byte layout of the integers written one after another, so a single copy does it.

// Load three consecutive 16-bit little-endian integers (6 bytes).
ALWAYS_INLINE void load_u16_x3(const uint8_t *at, uint16_t *first, uint16_t *second, uint16_t *third) {
    uint16_t values[3];
    copy_bytes(values, at, sizeof values);
    *first = values[0];
    *second = values[1];
    *third = values[2];
}

// Store three consecutive 16-bit little-endian integers (6 bytes).
ALWAYS_INLINE void store_u16_x3(uint8_t *at, uint16_t first, uint16_t second, uint16_t third) {
    const uint16_t values[3] = {first, second, third};
    copy_bytes(at, values, sizeof values);
}

// Store two consecutive 32-bit little-endian integers (8 bytes).
ALWAYS_INLINE void store_u32_x2(uint8_t *at, uint32_t first, uint32_t second) {
    const uint32_t values[2] = {first, second};
    copy_bytes(at, values, sizeof values);
}

// Store three consecutive 32-bit little-endian integers (12 bytes).
ALWAYS_INLINE void store_u32_x3(uint8_t *at, uint32_t first, uint32_t second, uint32_t third) {
    const uint32_t values[3] = {first, second, third};
    copy_bytes(at, values, sizeof values);
}

// Store three consecutive 64-bit little-endian integers (24 bytes).
ALWAYS_INLINE void store_u64_x3(uint8_t *at, uint64_t first, uint64_t second, uint64_t third) {
    const uint64_t values[3] = {first, second, third};
    copy_bytes(at, values, sizeof values);
}

#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error "lzfse-zazzcat assumes a little-endian CPU (Apple Silicon, x86-64, other little-endian ARM)"
#endif

#endif
