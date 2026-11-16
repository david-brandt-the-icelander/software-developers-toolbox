// The growable output buffer (lzfse_zazzcat_buffer, declared in the public header) and its append helpers.
#ifndef LZFSE_ZAZZCAT_BUFFER_H
#define LZFSE_ZAZZCAT_BUFFER_H

#include "common.h"

// Make room for at least `extra` more bytes after `buffer->length`, growing the capacity geometrically.
// Returns false when memory runs out (the buffer is left unchanged).
bool buffer_reserve(lzfse_zazzcat_buffer *buffer, size_t extra);

// Append `count` bytes; returns false when memory runs out.
bool buffer_append(lzfse_zazzcat_buffer *buffer, const void *bytes, size_t count);

// Append `count` bytes into capacity the caller has already reserved.
ALWAYS_INLINE void buffer_append_reserved(lzfse_zazzcat_buffer *buffer, const void *bytes, size_t count) {
    copy_bytes(buffer->data + buffer->length, bytes, count);
    buffer->length += count;
}

// Append one byte into reserved capacity. Fine for cold code; hot loops write through a local cursor instead
// (as the LZVN encoder does): a store through a `uint8_t *` may change any object, `buffer` included, so after
// every push the compiler must reload `data` and `length`.
ALWAYS_INLINE void buffer_push_reserved(lzfse_zazzcat_buffer *buffer, uint8_t byte) {
    buffer->data[buffer->length++] = byte;
}

// Append a little-endian 32-bit integer into reserved capacity.
ALWAYS_INLINE void buffer_push_u32_reserved(lzfse_zazzcat_buffer *buffer, uint32_t value) {
    store_u32(buffer->data + buffer->length, value);
    buffer->length += 4;
}

// Append two little-endian 32-bit integers into reserved capacity.
ALWAYS_INLINE void buffer_push_u32_x2_reserved(lzfse_zazzcat_buffer *buffer, uint32_t first, uint32_t second) {
    store_u32_x2(buffer->data + buffer->length, first, second);
    buffer->length += 8;
}

// Append a little-endian 64-bit integer into reserved capacity.
ALWAYS_INLINE void buffer_push_u64_reserved(lzfse_zazzcat_buffer *buffer, uint64_t value) {
    store_u64(buffer->data + buffer->length, value);
    buffer->length += 8;
}

#endif
