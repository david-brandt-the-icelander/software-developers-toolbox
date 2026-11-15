// LZFSE stream decoder: walks the blocks of a stream and writes their output into one buffer.
#include "bits.h"
#include "copy.h"
#include "format.h"
#include "fse.h"
#include "lzvn.h"

// Decoding tables and the literal buffer, reused across blocks and calls.
struct lzfse_zazzcat_decoder {
    fse_literal_entry literal_table[LITERAL_STATES];
    // The three value tables, back to back, so the token loop can reach all of them from one pointer:
    // literal lengths at [0, 64), match lengths at [64, 128), distances at [128, 384).
    fse_value_entry value_tables[LITERAL_LENGTH_STATES + MATCH_LENGTH_STATES + DISTANCE_STATES];
    // A block's decoded literals; grows to the largest block seen (at most 40,000 literals plus slack).
    uint8_t *literals;
    size_t literals_capacity;
};

lzfse_zazzcat_decoder *lzfse_zazzcat_decoder_create(void) {
    // Not zeroed: as in lzfse_zazzcat_decode_into, only the literal buffer fields need a starting value. The FSE
    // tables are rebuilt for every LZFSE block before anything reads them.
    lzfse_zazzcat_decoder *decoder = zz_malloc(sizeof *decoder);
    if (decoder == NULL) return NULL;
    decoder->literals = NULL;
    decoder->literals_capacity = 0;
    return decoder;
}

void lzfse_zazzcat_decoder_destroy(lzfse_zazzcat_decoder *decoder) {
    if (decoder == NULL) return;
    zz_free(decoder->literals);
    zz_free(decoder);
}

// Parse the v1 or v2 block header at `offset`.
static lzfse_zazzcat_status header_at(const uint8_t *source, size_t source_length, size_t offset, block_header *header,
                                      size_t *header_size) {
    if (load_u32(source + offset) == MAGIC_LZFSE_V1) {
        return parse_v1_header(source + offset, source_length - offset, header, header_size);
    }
    return parse_v2_header(source + offset, source_length - offset, header, header_size);
}

lzfse_zazzcat_status lzfse_zazzcat_decoded_length(const void *source_bytes, size_t source_length, size_t *length) {
    const uint8_t *source = source_bytes;
    size_t total = 0, offset = 0;
    for (;;) {
        uint32_t magic, block_decoded, payload;
        lzfse_zazzcat_status status = read_u32_at(source, source_length, offset, &magic);
        if (status != LZFSE_ZAZZCAT_OK) return status;
        // Each case computes the block's decoded size and the offset of the next block (in 64 bits, so a hostile
        // size cannot wrap around).
        uint64_t next;
        if (magic == MAGIC_END_OF_STREAM) {
            *length = total;
            return LZFSE_ZAZZCAT_OK;
        } else if (magic == MAGIC_UNCOMPRESSED) {
            if ((status = read_u32_at(source, source_length, offset + 4, &block_decoded))) return status;
            next = (uint64_t)offset + UNCOMPRESSED_HEADER_SIZE + block_decoded;
        } else if (magic == MAGIC_LZVN) {
            if ((status = read_u32_at(source, source_length, offset + 4, &block_decoded))) return status;
            if ((status = read_u32_at(source, source_length, offset + 8, &payload))) return status;
            next = (uint64_t)offset + LZVN_HEADER_SIZE + payload;
        } else if (magic == MAGIC_LZFSE_V1 || magic == MAGIC_LZFSE_V2) {
            block_header header;
            size_t header_size;
            if ((status = header_at(source, source_length, offset, &header, &header_size))) return status;
            block_decoded = header.decoded_size;
            next = (uint64_t)offset + header_size + header.literal_payload_size + header.token_payload_size;
        } else {
            return LZFSE_ZAZZCAT_CORRUPT;
        }
        if (total + block_decoded < total) return LZFSE_ZAZZCAT_CORRUPT; // overflow
        total += block_decoded;
        if (next > source_length) return LZFSE_ZAZZCAT_TRUNCATED;
        offset = (size_t)next;
    }
}

// Build the literal table and decode the block's literals (a multiple of 4) into the literal buffer.
// `source` ends where the literal stream ends; the stream may read back into earlier input, as Apple's does.
static NOINLINE lzfse_zazzcat_status decode_literals(lzfse_zazzcat_decoder *d, const block_header *h,
                                                     const uint8_t *source, size_t source_length) {
    lzfse_zazzcat_status status = fse_literal_table(h->literal_frequencies, LITERAL_STATES, d->literal_table);
    if (status != LZFSE_ZAZZCAT_OK) return status;
    size_t count = ((size_t)h->literal_count + 3) & ~(size_t)3;
    // Only grow the buffer: every literal up to `count` is overwritten below and the slack is never output.
    if (d->literals_capacity < count + COPY_SLACK) {
        uint8_t *grown = zz_realloc(d->literals, count + COPY_SLACK);
        if (grown == NULL) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
        d->literals = grown;
        d->literals_capacity = count + COPY_SLACK;
    }
    bit_reader reader; // bit_reader_init sets every field when it succeeds
    if ((status = bit_reader_init(&reader, source, source_length, h->literal_padding_bits))) return status;
    const fse_literal_entry *table = d->literal_table;
    uint32_t state0 = h->literal_states[0], state1 = h->literal_states[1];
    uint32_t state2 = h->literal_states[2], state3 = h->literal_states[3];
    uint8_t *out = d->literals;
    for (size_t i = 0; i < count; i += 4) {
        if ((status = bit_reader_refill(&reader))) return status;
        // Gather the four literals in a register and store them with one write. Storing them byte by byte would
        // force the compiler to keep each later table lookup behind the earlier stores (in C, a byte store may
        // change any memory, including the table).
        uint32_t four = fse_decode_literal(&state0, table, LITERAL_STATES - 1, &reader);
        four |= (uint32_t)fse_decode_literal(&state1, table, LITERAL_STATES - 1, &reader) << 8;
        four |= (uint32_t)fse_decode_literal(&state2, table, LITERAL_STATES - 1, &reader) << 16;
        four |= (uint32_t)fse_decode_literal(&state3, table, LITERAL_STATES - 1, &reader) << 24;
        store_u32(out + i, four);
    }
    return LZFSE_ZAZZCAT_OK;
}

// Execute a token the fast path declined: check its literal count and distance, then copy exactly. Kept out of
// line: it is rarely taken, and keeping its calls out of the hot loop keeps the decoder's state in registers.
// PRESERVE_REGISTERS makes this function save the registers it uses, so the loop need not spill around the call.
static COLD PRESERVE_REGISTERS lzfse_zazzcat_status decode_token_slow(uint8_t *output, size_t output_capacity,
                                                                      size_t position, const uint8_t *literals,
                                                                      size_t literals_available, size_t literal_length,
                                                                      size_t distance, size_t length) {
    if (literal_length > literals_available) return LZFSE_ZAZZCAT_CORRUPT;
    size_t match_start = position + literal_length;
    if (distance == 0 || distance > match_start) return LZFSE_ZAZZCAT_CORRUPT;
    if (match_start > output_capacity || length > output_capacity - match_start) {
        return LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL;
    }
    copy_bytes(output + position, literals, literal_length);
    if (distance == 1) {
        // A run of one byte (zeros, padding): memset speed.
        fill_bytes(output + match_start, output[match_start - 1], length);
    } else {
        copy_match_exact(output, match_start, distance, length);
    }
    return LZFSE_ZAZZCAT_OK;
}

// Decode the block's tokens and execute them into `output` from `*position`, advancing it.
static NOINLINE lzfse_zazzcat_status decode_tokens(lzfse_zazzcat_decoder *d, const block_header *h,
                                                   const uint8_t *token_stream, size_t token_stream_length,
                                                   uint8_t *output, size_t output_capacity, size_t *position_io) {
    lzfse_zazzcat_status status;
    if ((status = fse_value_table(h->literal_length_frequencies, LITERAL_LENGTH_SYMBOLS, LITERAL_LENGTH_STATES,
                                  literal_length_extra_bits, literal_length_base, d->value_tables)) ||
        (status =
             fse_value_table(h->match_length_frequencies, MATCH_LENGTH_SYMBOLS, MATCH_LENGTH_STATES,
                             match_length_extra_bits, match_length_base, d->value_tables + LITERAL_LENGTH_STATES)) ||
        (status = fse_value_table(h->distance_frequencies, DISTANCE_SYMBOLS, DISTANCE_STATES, distance_extra_bits,
                                  distance_base, d->value_tables + LITERAL_LENGTH_STATES + MATCH_LENGTH_STATES))) {
        return status;
    }
    bit_reader reader; // bit_reader_init sets every field when it succeeds
    if ((status = bit_reader_init(&reader, token_stream, token_stream_length, h->token_padding_bits))) {
        return status;
    }
    uint32_t literal_length_state = h->literal_length_state, match_length_state = h->match_length_state;
    uint32_t distance_state = h->distance_state;
    size_t literal_position = 0;
    size_t literal_count = ((size_t)h->literal_count + 3) & ~(size_t)3;
    size_t position = *position_io;
    // 0 until a token sets it, so a block starting with "repeat the previous distance" is rejected.
    size_t previous_distance = 0;
    // Locals rather than reads through `h` and `d`: in C, every byte stored into the output might change memory
    // that pointers point to, so values read through pointers would be reloaded after each token.
    const uint8_t *literals = d->literals;
    const fse_value_entry *tables = d->value_tables;
    // The fast path requires `literal_position + literal_length + 16 <= literal_limit`, which also checks that the
    // token stays within this block's literals (the buffer has COPY_SLACK >= 16 spare bytes after them).
    size_t literal_limit = literal_count + 16;
    // Each token's table entries are looked up at the end of the previous token, before its copies: the states
    // then stay in registers (otherwise the compiler postpones the state updates past the copies and spills
    // their pieces), and the loads overlap the copies. Masking keeps every index in range.
    const fse_value_entry *match_table = tables + LITERAL_LENGTH_STATES;
    const fse_value_entry *distance_table = match_table + MATCH_LENGTH_STATES;
    fse_value_entry literal_length_entry = tables[literal_length_state & (LITERAL_LENGTH_STATES - 1)];
    fse_value_entry match_length_entry = match_table[match_length_state & (MATCH_LENGTH_STATES - 1)];
    fse_value_entry distance_entry = distance_table[distance_state & (DISTANCE_STATES - 1)];
    for (uint32_t tokens_left = h->token_count; tokens_left > 0; tokens_left--) {
        if ((status = bit_reader_refill(&reader))) return status;
        size_t literal_length = fse_decode_value_from(literal_length_entry, &literal_length_state, &reader);
        size_t length = fse_decode_value_from(match_length_entry, &match_length_state, &reader);
        size_t coded_distance = fse_decode_value_from(distance_entry, &distance_state, &reader);
        literal_length_entry = tables[literal_length_state & (LITERAL_LENGTH_STATES - 1)];
        match_length_entry = match_table[match_length_state & (MATCH_LENGTH_STATES - 1)];
        distance_entry = distance_table[distance_state & (DISTANCE_STATES - 1)];
        // A coded distance of 0 means "the same distance as the previous token".
        size_t distance = coded_distance != 0 ? coded_distance : previous_distance;
        previous_distance = distance;
        if (!decode_token_fast(output, output_capacity, position, literals, literal_limit, literal_position,
                               literal_length, distance, length)) {
            status = decode_token_slow(output, output_capacity, position, literals + literal_position,
                                       literal_count - literal_position, literal_length, distance, length);
            if (status != LZFSE_ZAZZCAT_OK) return status;
        }
        literal_position += literal_length;
        position += literal_length + length;
    }
    *position_io = position;
    return LZFSE_ZAZZCAT_OK;
}

// Decode a stream into `output` using the decoder's tables and literal buffer.
static lzfse_zazzcat_status decode_with(lzfse_zazzcat_decoder *d, const uint8_t *source, size_t source_length,
                                        uint8_t *output, size_t output_capacity, size_t *decoded_length) {
    size_t offset = 0, position = 0;
    for (;;) {
        uint32_t magic, block_decoded, payload_size;
        lzfse_zazzcat_status status = read_u32_at(source, source_length, offset, &magic);
        if (status != LZFSE_ZAZZCAT_OK) return status;
        if (magic == MAGIC_END_OF_STREAM) {
            *decoded_length = position;
            return LZFSE_ZAZZCAT_OK;
        } else if (magic == MAGIC_UNCOMPRESSED) {
            if ((status = read_u32_at(source, source_length, offset + 4, &block_decoded))) return status;
            size_t body = offset + UNCOMPRESSED_HEADER_SIZE;
            if (block_decoded > source_length - body) return LZFSE_ZAZZCAT_TRUNCATED;
            if (block_decoded > output_capacity - position) return LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL;
            copy_bytes(output + position, source + body, block_decoded);
            position += block_decoded;
            offset = body + block_decoded;
        } else if (magic == MAGIC_LZVN) {
            if ((status = read_u32_at(source, source_length, offset + 4, &block_decoded))) return status;
            if ((status = read_u32_at(source, source_length, offset + 8, &payload_size))) return status;
            size_t payload = offset + LZVN_HEADER_SIZE;
            if (payload_size > source_length - payload) return LZFSE_ZAZZCAT_TRUNCATED;
            if (block_decoded > output_capacity - position) return LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL;
            size_t end = position + block_decoded;
            if ((status = lzvn_decode_block(source + payload, payload_size, output, position, end))) return status;
            position = end;
            offset = payload + payload_size;
        } else if (magic == MAGIC_LZFSE_V1 || magic == MAGIC_LZFSE_V2) {
            block_header header;
            size_t header_size;
            if ((status = header_at(source, source_length, offset, &header, &header_size))) return status;
            if ((status = block_header_validate(&header))) return status;
            // Payload sizes are below 2^32 each, so these sums cannot wrap around in 64-bit size_t.
            size_t literal_start = offset + header_size;
            size_t token_start = literal_start + header.literal_payload_size;
            size_t token_end = token_start + header.token_payload_size;
            if (token_end > source_length) return LZFSE_ZAZZCAT_TRUNCATED;
            if ((status = decode_literals(d, &header, source, token_start))) return status;
            if ((status = decode_tokens(d, &header, source + token_start, header.token_payload_size, output,
                                        output_capacity, &position))) {
                return status;
            }
            offset = token_end;
        } else {
            return LZFSE_ZAZZCAT_CORRUPT;
        }
    }
}

lzfse_zazzcat_status lzfse_zazzcat_decoder_decode(lzfse_zazzcat_decoder *decoder, const void *source,
                                                  size_t source_length, void *destination, size_t destination_capacity,
                                                  size_t *decoded_length) {
    return decode_with(decoder, source, source_length, destination, destination_capacity, decoded_length);
}

lzfse_zazzcat_status lzfse_zazzcat_decode_into(const void *source, size_t source_length, void *destination,
                                               size_t destination_capacity, size_t *decoded_length) {
    // The tables (about 7 KB) live on the stack; only the literal buffer is allocated.
    lzfse_zazzcat_decoder decoder;
    // Not cleared: only the literal buffer fields need a starting value. The FSE tables are rebuilt by
    // fse_literal_table and fse_value_table for every LZFSE block before anything reads them, and streams of
    // LZVN or uncompressed blocks never read them at all.
    // clear_bytes(&decoder, sizeof decoder);
    decoder.literals = NULL;
    decoder.literals_capacity = 0;
    lzfse_zazzcat_status status =
        decode_with(&decoder, source, source_length, destination, destination_capacity, decoded_length);
    zz_free(decoder.literals);
    return status;
}

lzfse_zazzcat_status lzfse_zazzcat_decode(const void *source, size_t source_length, size_t limit,
                                          lzfse_zazzcat_buffer *output) {
    size_t length;
    lzfse_zazzcat_status status = lzfse_zazzcat_decoded_length(source, source_length, &length);
    if (status != LZFSE_ZAZZCAT_OK) return status;
    if (length > limit) return LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL;
    if (!buffer_reserve(output, length)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    size_t decoded;
    status = lzfse_zazzcat_decode_into(source, source_length, output->data + output->length, length, &decoded);
    // The headers promised `length` bytes, so running out of room or ending early means the stream lies.
    if (status == LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL || (status == LZFSE_ZAZZCAT_OK && decoded != length)) {
        return LZFSE_ZAZZCAT_CORRUPT;
    }
    if (status != LZFSE_ZAZZCAT_OK) return status;
    output->length += length;
    return LZFSE_ZAZZCAT_OK;
}
