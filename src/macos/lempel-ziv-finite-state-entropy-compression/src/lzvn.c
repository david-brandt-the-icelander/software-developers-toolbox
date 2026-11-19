// LZVN decoder and encoder.
//
// An LZVN stream is a sequence of opcodes. Each opcode carries some literal bytes (copied from the stream) and/or
// a match (bytes copied from earlier output), in one of several compact layouts chosen by the opcode byte.
#include "lzvn.h"
#include "buffer.h"

#include "copy.h"

#define END_OF_STREAM_OPCODE 0x06
#define MAX_LZVN_DISTANCE    0xFFFF
#define MAX_HASH_BITS        14

// The kinds of LZVN opcode. Each opcode byte belongs to exactly one kind (see opcode_kinds), which fixes how many
// bytes the opcode takes and where its literal length, match length and distance come from.
typedef enum lzvn_kind {
    // 0-3 literals, match length 3-10, distance in 11 bits (2 bytes).
    SMALL_DISTANCE,
    // 0-3 literals, match length 3-34, distance in 14 bits (3 bytes).
    MEDIUM_DISTANCE,
    // 0-3 literals, match length 3-10, distance in 16 bits (3 bytes).
    LARGE_DISTANCE,
    // 1-3 literals, match length 3-10, the previous distance (1 byte).
    PREVIOUS_DISTANCE,
    // Match length 1-15 at the previous distance (1 byte), or 16 + the next byte (2 bytes).
    SMALL_MATCH,
    LARGE_MATCH,
    // 1-15 literals (1 byte), or 16 + the next byte (2 bytes).
    SMALL_LITERALS,
    LARGE_LITERALS,
    // Does nothing (1 byte).
    NOP,
    // Ends the stream (8 bytes).
    END_OF_STREAM,
    // Reserved: the stream is corrupt.
    UNDEFINED,
} lzvn_kind;

// Shorthands for the table below.
#define S_ SMALL_DISTANCE
#define M_ MEDIUM_DISTANCE
#define L_ LARGE_DISTANCE
#define P_ PREVIOUS_DISTANCE
#define U_ UNDEFINED
#define SL SMALL_LITERALS
#define LL LARGE_LITERALS
#define SM SMALL_MATCH
#define LM LARGE_MATCH
// The usual pattern of 8 opcodes: the low 3 bits 0-5 give a small distance, 6 the previous one, 7 a large one.
#define EIGHT S_, S_, S_, S_, S_, S_, P_, L_

// The kind of each opcode byte, one row of 16 per value of the high 4 bits. Looking the kind up in a table and
// switching on it takes a few instructions per opcode, where testing the byte against each range in turn took
// dozens.
// clang-format off
static const uint8_t opcode_kinds[256] = {
    S_, S_, S_, S_, S_, S_, END_OF_STREAM, L_, S_, S_, S_, S_, S_, S_, NOP, L_, // 0x00: no literals
    S_, S_, S_, S_, S_, S_, NOP, L_, S_, S_, S_, S_, S_, S_, U_, L_,            // 0x10
    S_, S_, S_, S_, S_, S_, U_, L_, S_, S_, S_, S_, S_, S_, U_, L_,             // 0x20
    S_, S_, S_, S_, S_, S_, U_, L_, S_, S_, S_, S_, S_, S_, U_, L_,             // 0x30
    EIGHT, EIGHT,                                                               // 0x40: 1 literal
    EIGHT, EIGHT,                                                               // 0x50
    EIGHT, EIGHT,                                                               // 0x60
    U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_,             // 0x70: reserved
    EIGHT, EIGHT,                                                               // 0x80: 2 literals
    EIGHT, EIGHT,                                                               // 0x90
    M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_,             // 0xA0: medium distance
    M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_, M_,             // 0xB0
    EIGHT, EIGHT,                                                               // 0xC0: 3 literals
    U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_, U_,             // 0xD0: reserved
    LL, SL, SL, SL, SL, SL, SL, SL, SL, SL, SL, SL, SL, SL, SL, SL,             // 0xE0: literals only
    LM, SM, SM, SM, SM, SM, SM, SM, SM, SM, SM, SM, SM, SM, SM, SM,             // 0xF0: previous distance
};
// clang-format on

#undef S_
#undef M_
#undef L_
#undef P_
#undef U_
#undef SL
#undef LL
#undef SM
#undef LM
#undef EIGHT

// Execute an opcode the fast path declined, with exact copies: `literal_length` literals from `literals` (all in
// the stream), then `match_length` bytes (0 for none) from `distance` back, to `output + position`. Kept out of
// line, as decode_token_slow is in decode.c: its calls (copy_bytes is memcpy) would otherwise sit inside the
// decoding loop, which then keeps its values in the few registers a call preserves, or on the stack.
static COLD PRESERVE_REGISTERS lzfse_zazzcat_status decode_opcode_slow(uint8_t *output, size_t output_capacity,
                                                                       size_t position, const uint8_t *literals,
                                                                       size_t literal_length, size_t distance,
                                                                       size_t match_length) {
    if (literal_length > output_capacity - position) return LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL;
    copy_bytes(output + position, literals, literal_length);
    position += literal_length;
    if (match_length == 0) return LZFSE_ZAZZCAT_OK;
    if (distance == 0 || distance > position) return LZFSE_ZAZZCAT_CORRUPT;
    if (match_length > output_capacity - position) return LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL;
    copy_match_exact(output, position, distance, match_length);
    return LZFSE_ZAZZCAT_OK;
}

// Run opcodes from `source` into `output` starting at `position` until end-of-stream. Store the number of source
// bytes used and the output end position.
static lzfse_zazzcat_status decode_run(const uint8_t *source, size_t source_length, uint8_t *output,
                                       size_t output_capacity, size_t position, size_t *bytes_read,
                                       size_t *output_end) {
    size_t at = 0;
    size_t previous_distance = 0;
    for (;;) {
        if (at >= source_length) return LZFSE_ZAZZCAT_TRUNCATED;
        const uint8_t *opcode = source + at;
        size_t op = opcode[0];
        size_t available = source_length - at;
        // A literal-only opcode has match length 0; its distance (1) is never used.
        size_t opcode_length, literal_length, match_length, distance;
        switch ((lzvn_kind)opcode_kinds[op]) {
        case SMALL_DISTANCE:
            if (available < 2) return LZFSE_ZAZZCAT_TRUNCATED;
            opcode_length = 2;
            literal_length = op >> 6;
            match_length = ((op >> 3) & 7) + 3;
            distance = ((op & 7) << 8) | opcode[1];
            break;
        case MEDIUM_DISTANCE: {
            if (available < 3) return LZFSE_ZAZZCAT_TRUNCATED;
            size_t word = load_u16(opcode + 1);
            opcode_length = 3;
            literal_length = (op >> 3) & 3;
            match_length = (((op & 7) << 2) | (word & 3)) + 3;
            distance = word >> 2;
            break;
        }
        case LARGE_DISTANCE:
            if (available < 3) return LZFSE_ZAZZCAT_TRUNCATED;
            opcode_length = 3;
            literal_length = op >> 6;
            match_length = ((op >> 3) & 7) + 3;
            distance = load_u16(opcode + 1);
            break;
        case PREVIOUS_DISTANCE:
            opcode_length = 1;
            literal_length = op >> 6;
            match_length = ((op >> 3) & 7) + 3;
            distance = previous_distance;
            break;
        case SMALL_MATCH:
            opcode_length = 1;
            literal_length = 0;
            match_length = op & 0x0F;
            distance = previous_distance;
            break;
        case LARGE_MATCH:
            if (available < 2) return LZFSE_ZAZZCAT_TRUNCATED;
            opcode_length = 2;
            literal_length = 0;
            match_length = 16 + (size_t)opcode[1];
            distance = previous_distance;
            break;
        case SMALL_LITERALS:
            opcode_length = 1;
            literal_length = op & 0x0F;
            match_length = 0;
            distance = 1;
            break;
        case LARGE_LITERALS:
            if (available < 2) return LZFSE_ZAZZCAT_TRUNCATED;
            opcode_length = 2;
            literal_length = 16 + (size_t)opcode[1];
            match_length = 0;
            distance = 1;
            break;
        case NOP:
            at += 1;
            continue;
        case END_OF_STREAM:
            // The opcode plus 7 zero bytes.
            if (available < 8) return LZFSE_ZAZZCAT_TRUNCATED;
            *bytes_read = at + 8;
            *output_end = position;
            return LZFSE_ZAZZCAT_OK;
        default:
            return LZFSE_ZAZZCAT_CORRUPT;
        }
        // Every opcode but end-of-stream needs at least one more byte after its literals.
        if (available <= opcode_length + literal_length) return LZFSE_ZAZZCAT_TRUNCATED;
        at += opcode_length;
        // Fast path: the shared wide-copy token routine, which makes no calls. The literals sit in the stream
        // right after the opcode. A literal-only opcode passes match length 0 and distance 1, which always passes
        // its distance check because it carries at least one literal. Anything near either buffer's end, or
        // invalid (such as a previous distance of 0), goes to decode_opcode_slow instead.
        if (!decode_token_fast(output, output_capacity, position, source, source_length, at, literal_length, distance,
                               match_length)) {
            lzfse_zazzcat_status status = decode_opcode_slow(output, output_capacity, position, source + at,
                                                             literal_length, distance, match_length);
            if (status != LZFSE_ZAZZCAT_OK) return status;
        }
        at += literal_length;
        position += literal_length + match_length;
        // Every opcode with a match has a match length of at least 1.
        if (match_length != 0) previous_distance = distance;
    }
}

lzfse_zazzcat_status lzvn_decode_block(const uint8_t *payload, size_t payload_length, uint8_t *output, size_t position,
                                       size_t end) {
    size_t used, output_end;
    lzfse_zazzcat_status status = decode_run(payload, payload_length, output, end, position, &used, &output_end);
    if (status != LZFSE_ZAZZCAT_OK) return status;
    if (used != payload_length || output_end != end) return LZFSE_ZAZZCAT_CORRUPT;
    return LZFSE_ZAZZCAT_OK;
}

lzfse_zazzcat_status lzfse_zazzcat_lzvn_decode(const void *source, size_t source_length, void *destination,
                                               size_t destination_capacity, size_t *decoded_length) {
    size_t used;
    return decode_run(source, source_length, destination, destination_capacity, 0, &used, decoded_length);
}

// Largest match length a short opcode (3-bit length) can carry alongside `literal_count` literals: some longer
// combinations would collide with reserved opcodes.
static size_t max_short_match(size_t literal_count) {
    static const size_t limits[4] = {10, 8, 6, 4};
    return limits[literal_count];
}

// The functions below append opcodes at `at`, a cursor into the output's reserved space, and return the advanced
// cursor; lzvn_encode_with_table sets the buffer's length once at the end. Pushing through the buffer would store
// `length` and load `data` and `length` again after every byte, since a byte store may change any memory.

// Append literal-only opcodes for `count` bytes at `literals`, of which `readable` bytes may be read. When at least
// 16 bytes beyond each piece may be read, the piece is copied with wild_copy, which can write up to 15 bytes past
// it; the output's reservation (2 bytes per input byte plus 64) leaves room, and later output overwrites them.
// Nearer the end, copy_exact_inline copies exactly. Always inlined: with that copy inside, the compiler would
// otherwise make this a separate function, and the encoder loop would call it for every literal run.
ALWAYS_INLINE uint8_t *emit_literals(uint8_t *at, const uint8_t *literals, size_t count, size_t readable) {
    while (count > 0) {
        size_t n = min_size(count, 271);
        if (n >= 16) {
            // Opcode 0xE0 then the length byte, built as one little-endian value and stored at once.
            store_u16(at, (uint16_t)(0xE0 | (n - 16) << 8));
            at += 2;
        } else {
            *at++ = (uint8_t)(0xE0 | n);
        }
        if (n + 16 <= readable) {
            wild_copy(at, literals, n);
        } else {
            copy_exact_inline(at, literals, n);
        }
        at += n;
        literals += n;
        readable -= n;
        count -= n;
    }
    return at;
}

// Append match-only opcodes that repeat the previous distance for `length` bytes.
static uint8_t *emit_repeat(uint8_t *at, size_t length) {
    while (length > 0) {
        size_t n = min_size(length, 271);
        if (n >= 16) {
            // Opcode 0xF0 then the length byte, stored at once as in emit_literals.
            store_u16(at, (uint16_t)(0xF0 | (n - 16) << 8));
            at += 2;
        } else {
            *at++ = (uint8_t)(0xF0 | n);
        }
        length -= n;
    }
    return at;
}

// Append the opcodes for `literal_count` literals followed by a match of `length` (>= 3) bytes at `distance`
// (<= 65535). Up to 3 literals ride along in the match opcode; more go in a literal opcode of their own.
// `readable` is how many bytes may be read from `literals` (to the end of the input).
static uint8_t *emit_token(uint8_t *at, const uint8_t *literals, size_t literal_count, size_t length, size_t distance,
                           size_t previous_distance, size_t readable) {
    // From here on `literal_count` counts only the literals that ride along in the match opcode.
    if (literal_count > 3) {
        at = emit_literals(at, literals, literal_count, readable);
        literal_count = 0;
    }
    if (distance == previous_distance && literal_count == 0) return emit_repeat(at, length);
    size_t first;
    if (distance == previous_distance) {
        first = min_size(length, max_short_match(literal_count));
        *at++ = (uint8_t)((literal_count << 6) | ((first - 3) << 3) | 6);
    } else if (distance < 16384 && (length > max_short_match(literal_count) || distance >= 1536)) {
        // The 3-byte opcodes are built as one little-endian value (byte 0 in the low bits) and written with a single
        // 4-byte store instead of three byte stores. The 4th byte is garbage: like the literal copy below, it lands
        // in reserved space that the next output overwrites (at least the 8-byte end-of-stream trailer follows).
        first = min_size(length, 34);
        size_t word = (distance << 2) | ((first - 3) & 3);
        size_t opcode = 0xA0 | (literal_count << 3) | ((first - 3) >> 2);
        store_u32(at, (uint32_t)(opcode | word << 8));
        at += 3;
    } else if (distance < 1536) {
        first = min_size(length, max_short_match(literal_count));
        size_t opcode = (literal_count << 6) | ((first - 3) << 3) | (distance >> 8);
        store_u16(at, (uint16_t)(opcode | (distance & 0xFF) << 8));
        at += 2;
    } else {
        first = min_size(length, max_short_match(literal_count));
        size_t opcode = (literal_count << 6) | ((first - 3) << 3) | 7;
        store_u32(at, (uint32_t)(opcode | distance << 8));
        at += 3;
    }
    if (literal_count > 0) {
        // Copy 4 bytes, a constant size the compiler turns into one load and one store, and keep 1 to 3 of them.
        // Reading 4 is safe: the match of at least 3 bytes follows the literals. The extra written bytes are
        // overwritten by the next output (at least the 8-byte end-of-stream trailer follows).
        store_u32(at, load_u32(literals));
        at += literal_count;
    }
    return emit_repeat(at, length - first);
}

// Hash 4 bytes into a table index (multiplicative hashing: the top bits of the product).
ALWAYS_INLINE size_t lzvn_hash(uint32_t four_bytes, uint32_t shift) {
    return (size_t)((four_bytes * 0x9E3779B1u) >> shift);
}

lzfse_zazzcat_status lzvn_encode_with_table(const uint8_t *source, size_t n, lzfse_zazzcat_buffer *out,
                                            lzvn_hash_table *table) {
    if (!buffer_reserve(out, lzvn_encode_bound(n))) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    // Small inputs get small tables: about one bucket per 4 input bytes, between 2^8 and 2^14.
    uint32_t bits = n == 0 ? 0 : (uint32_t)(64 - __builtin_clzll((unsigned long long)n));
    bits = bits >= 2 ? bits - 2 : 0;
    if (bits < 8) bits = 8;
    if (bits > MAX_HASH_BITS) bits = MAX_HASH_BITS;
    size_t entries = (size_t)1 << bits;
    if (table->capacity < entries) {
        uint32_t *grown = zz_realloc(table->entries, entries * sizeof *grown);
        if (grown == NULL) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
        table->entries = grown;
        table->capacity = entries;
    }
    // Entries hold position + 1, so 0 means empty.
    uint32_t *slots = table->entries;
    clear_bytes(slots, entries * sizeof *slots);
    uint32_t shift = 32 - bits;
    uint8_t *at = out->data + out->length;
    size_t i = 0, literal_start = 0, previous_distance = 0;
    while (i + 4 <= n) {
        uint32_t four = load_u32(source + i);
        size_t h = lzvn_hash(four, shift);
        size_t candidate = slots[h];
        slots[h] = (uint32_t)(i + 1);
        size_t best_length = 0, best_distance = 0;
        if (previous_distance > 0 && previous_distance <= i) {
            size_t length = match_length(source, i - previous_distance, i, n);
            if (length >= 3) {
                best_length = length;
                best_distance = previous_distance;
            }
        }
        if (candidate > 0) {
            size_t earlier = candidate - 1;
            size_t distance = i - earlier;
            if (distance <= MAX_LZVN_DISTANCE && load_u32(source + earlier) == four) {
                size_t length = 4 + match_length(source, earlier + 4, i + 4, n);
                if (length > best_length + 1) {
                    best_length = length;
                    best_distance = distance;
                }
            }
        }
        if (best_length < 3) {
            i++;
            continue;
        }
        // Extend backwards over pending literals.
        size_t back = match_length_backward(source, i - best_distance, i, i - literal_start);
        size_t start = i - back;
        size_t length = best_length + back;
        at = emit_token(at, source + literal_start, start - literal_start, length, best_distance, previous_distance,
                        n - literal_start);
        previous_distance = best_distance;
        size_t end = start + length;
        for (size_t j = i + 1; j + 4 <= n && j < end; j++)
            slots[lzvn_hash(load_u32(source + j), shift)] = (uint32_t)(j + 1);
        i = end;
        literal_start = end;
    }
    at = emit_literals(at, source + literal_start, n - literal_start, n - literal_start);
    static const uint8_t trailer[8] = {END_OF_STREAM_OPCODE, 0, 0, 0, 0, 0, 0, 0};
    copy_bytes(at, trailer, 8);
    out->length = (size_t)(at + 8 - out->data);
    return LZFSE_ZAZZCAT_OK;
}

lzfse_zazzcat_status lzfse_zazzcat_lzvn_encode(const void *source, size_t source_length, lzfse_zazzcat_buffer *output) {
    lzvn_hash_table table = {NULL, 0};
    lzfse_zazzcat_status status = lzvn_encode_with_table(source, source_length, output, &table);
    zz_free(table.entries);
    return status;
}
