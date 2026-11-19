// LZVN: Apple's byte-oriented LZ77 format, used for small LZFSE blocks and for decmpfs types 7 and 8.
#ifndef LZFSE_ZAZZCAT_LZVN_H
#define LZFSE_ZAZZCAT_LZVN_H

#include "common.h"

// Decode an LZVN payload into `output[position .. end)`, where `output[0 .. position)` is earlier output that
// matches may refer to. The payload must end with the end-of-stream opcode exactly at its last byte and fill the
// range exactly.
lzfse_zazzcat_status lzvn_decode_block(const uint8_t *payload, size_t payload_length, uint8_t *output, size_t position,
                                       size_t end);

// A reusable hash table for the LZVN encoder (grown on demand, never shrunk).
typedef struct lzvn_hash_table {
    uint32_t *entries;
    size_t capacity;
} lzvn_hash_table;

// Room the LZVN encoder reserves for an `source_length`-byte input: every input byte costs at most 2 output bytes
// (a literal plus its share of an opcode), plus the trailer.
static inline size_t lzvn_encode_bound(size_t source_length) {
    return 2 * source_length + 64;
}

// Append `source` compressed as an LZVN payload (ending with the end-of-stream opcode) to `output`, using
// `table` for the match finder.
lzfse_zazzcat_status lzvn_encode_with_table(const uint8_t *source, size_t source_length, lzfse_zazzcat_buffer *output,
                                            lzvn_hash_table *table);

#endif
