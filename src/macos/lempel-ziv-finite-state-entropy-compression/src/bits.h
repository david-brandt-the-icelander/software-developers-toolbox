// Bit streams. The encoder writes bits forward, least significant bit first; the decoder reads them backwards
// from the end, so the last value written is the first one read.
#ifndef LZFSE_ZAZZCAT_BITS_H
#define LZFSE_ZAZZCAT_BITS_H

#include "buffer.h"

// Forward bit writer: bits gather in a 64-bit accumulator and whole bytes are stored at `next`, a cursor into an
// output buffer. The cursor is a field of the writer, normally a local variable, rather than the buffer's `length`:
// a byte store may change any memory the compiler cannot see the whole of, so after each flush it would have to
// reload a `length` kept behind a pointer. A local writer's fields stay in registers instead.
typedef struct bit_writer {
    uint64_t accumulator;
    uint32_t bit_count;
    // Where the next whole byte goes.
    uint8_t *next;
} bit_writer;

// Start writing at the end of `output`. The caller has reserved room for everything written before
// bit_writer_finish, and must not grow `output` in between (that could move its data).
ALWAYS_INLINE void bit_writer_start(bit_writer *writer, lzfse_zazzcat_buffer *output) {
    writer->accumulator = 0;
    writer->bit_count = 0;
    writer->next = output->data + output->length;
}

// Append the low `count` bits of `value` (`value` must have no higher bits set, so it is 0 when `count` is 0).
// The caller flushes before 64 bits accumulate, so the shift is always below 64.
ALWAYS_INLINE void bit_writer_push(bit_writer *writer, uint32_t count, uint64_t value) {
    writer->accumulator |= value << writer->bit_count;
    writer->bit_count += count;
}

// Move the whole bytes of the accumulator to the output, leaving at most 7 bits. At least 8 bytes must be reserved
// past `next`: all 8 accumulator bytes are stored (one fixed-size store, cheaper than a variable-length copy) and
// only the whole ones are kept.
ALWAYS_INLINE void bit_writer_flush(bit_writer *writer) {
    uint32_t bytes = writer->bit_count / 8;
    store_u64(writer->next, writer->accumulator);
    writer->next += bytes;
    // Shifting a 64-bit value by 64 is undefined in C, so the full-accumulator case is handled separately.
    writer->accumulator = bytes == 8 ? 0 : writer->accumulator >> (8 * bytes);
    writer->bit_count -= 8 * bytes;
}

// Write the remaining bits zero-padded to a whole byte (9 bytes must be reserved past `next`), set the length of
// `output` (the buffer given to bit_writer_start) to the end of what was written, and return the padding as a count
// in [-7, 0], which the block header records.
ALWAYS_INLINE int32_t bit_writer_finish(bit_writer *writer, lzfse_zazzcat_buffer *output) {
    bit_writer_flush(writer);
    int32_t padding = 0;
    if (writer->bit_count != 0) {
        *writer->next++ = (uint8_t)writer->accumulator;
        padding = (int32_t)writer->bit_count - 8;
    }
    output->length = (size_t)(writer->next - output->data);
    return padding;
}

// Backward bit reader. After each refill it holds 56 to 63 bits, as Apple's decoder does: enough for any one
// token's three values, or four literals.
typedef struct bit_reader {
    const uint8_t *buffer;
    // Index of the first buffer byte already loaded into the accumulator; bytes before it are still unread.
    size_t position;
    uint64_t accumulator;
    // Number of valid bits in the accumulator (its low bits).
    uint32_t bit_count;
} bit_reader;

// Start reading at the end of `buffer[0 .. length)`, whose last byte holds `-padding_bits` padding bits
// (`padding_bits` in [-7, 0]). Fails if the buffer is too short or has stray bits above the stated end.
ALWAYS_INLINE lzfse_zazzcat_status bit_reader_init(bit_reader *reader, const uint8_t *buffer, size_t length,
                                                   int32_t padding_bits) {
    size_t take = padding_bits == 0 ? 7 : 8;
    if (length < take) return LZFSE_ZAZZCAT_CORRUPT;
    size_t position = length - take;
    uint8_t bytes[8] = {0};
    copy_bytes(bytes, buffer + position, take);
    uint64_t accumulator = load_u64(bytes);
    uint32_t bit_count = (uint32_t)(8 * (int32_t)take + padding_bits);
    if (bit_count < 56 || (bit_count < 64 && (accumulator >> bit_count) != 0)) return LZFSE_ZAZZCAT_CORRUPT;
    reader->buffer = buffer;
    reader->position = position;
    reader->accumulator = accumulator;
    reader->bit_count = bit_count;
    return LZFSE_ZAZZCAT_OK;
}

// Load whole bytes from before the current position so that at least 56 bits are available. Fails if the stream
// would need bytes before its start. Reading 8 bytes at `start` stays in bounds: init consumed at least 7 bytes
// and the position only moves backwards.
//
// The accumulator's valid (low) bits are always the bytes at `position` onwards, as a little-endian load of
// `buffer + position` would give them. So after moving the position back, one load from the new position holds
// the new bytes below the old valid bits, and replaces masking in the new bytes, shifting the old ones up and
// merging them. The bits above the valid ones differ (they are bytes already read), but bit_reader_pull never
// looks at them. The early return matters for this: just after init with 7 bytes, a load at the position would
// read one byte past the end of the buffer.
ALWAYS_INLINE lzfse_zazzcat_status bit_reader_refill(bit_reader *reader) {
    uint32_t bytes = (63 - reader->bit_count) / 8;
    if (bytes == 0) return LZFSE_ZAZZCAT_OK;
    if (UNLIKELY(reader->position < bytes)) return LZFSE_ZAZZCAT_CORRUPT;
    reader->position -= bytes;
    reader->accumulator = load_u64(reader->buffer + reader->position);
    reader->bit_count += 8 * bytes;
    return LZFSE_ZAZZCAT_OK;
}

// Take the `count` most recently written bits (count <= 32; a refill must have made them available). Bits above
// the read position stay in the accumulator; refills shift them out, which keeps this off the critical path.
ALWAYS_INLINE uint32_t bit_reader_pull(bit_reader *reader, uint32_t count) {
    reader->bit_count -= count;
    return (uint32_t)((reader->accumulator >> reader->bit_count) & ((1ull << count) - 1));
}

#endif
