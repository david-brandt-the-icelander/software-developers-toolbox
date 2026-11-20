// The growable output buffer.
#include "buffer.h"

bool buffer_reserve(lzfse_zazzcat_buffer *buffer, size_t extra) {
    // An empty buffer gets memory even for 0 bytes, so `data + length` is never arithmetic on a NULL pointer
    // (which C does not allow).
    if (extra <= buffer->capacity - buffer->length && buffer->data != NULL) return true;
    if (extra > SIZE_MAX - buffer->length) return false;
    size_t needed = buffer->length + extra;
    if (needed < 64) needed = 64;
    // Grow geometrically (doubling), so a long series of appends costs few reallocations.
    size_t capacity = buffer->capacity > SIZE_MAX / 2 ? SIZE_MAX : buffer->capacity * 2;
    if (capacity < needed) capacity = needed;
    uint8_t *grown = zz_realloc(buffer->data, capacity);
    if (grown == NULL) return false;
    buffer->data = grown;
    buffer->capacity = capacity;
    return true;
}

bool buffer_append(lzfse_zazzcat_buffer *buffer, const void *bytes, size_t count) {
    if (!buffer_reserve(buffer, count)) return false;
    buffer_append_reserved(buffer, bytes, count);
    return true;
}

void lzfse_zazzcat_buffer_free(lzfse_zazzcat_buffer *buffer) {
    zz_free(buffer->data);
    buffer->data = NULL;
    buffer->length = 0;
    buffer->capacity = 0;
}
