// Value tables and block header parsing and writing.
#include "format.h"
#include "buffer.h"

const uint8_t literal_length_extra_bits[LITERAL_LENGTH_SYMBOLS] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                                                                   0, 0, 0, 0, 0, 0, 2, 3, 5, 8};
const uint32_t literal_length_base[LITERAL_LENGTH_SYMBOLS] = {0,  1,  2,  3,  4,  5,  6,  7,  8,  9,
                                                              10, 11, 12, 13, 14, 15, 16, 20, 28, 60};
const uint8_t match_length_extra_bits[MATCH_LENGTH_SYMBOLS] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                                                               0, 0, 0, 0, 0, 0, 3, 5, 8, 11};
const uint32_t match_length_base[MATCH_LENGTH_SYMBOLS] = {0,  1,  2,  3,  4,  5,  6,  7,  8,  9,
                                                          10, 11, 12, 13, 14, 15, 16, 24, 56, 312};
const uint8_t distance_extra_bits[DISTANCE_SYMBOLS] = {
    0,  0,  0,  0,  1,  1,  1,  1,  2,  2,  2,  2,  3,  3,  3,  3,  4,  4,  4,  4,  5,  5,
    5,  5,  6,  6,  6,  6,  7,  7,  7,  7,  8,  8,  8,  8,  9,  9,  9,  9,  10, 10, 10, 10,
    11, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, 14, 14, 14, 15, 15, 15, 15,
};
const uint32_t distance_base[DISTANCE_SYMBOLS] = {
    0,     1,     2,     3,     4,     6,     8,     10,    12,    16,    20,    24,     28,     36,     44,     52,
    60,    76,    92,    108,   124,   156,   188,   220,   252,   316,   380,   444,    508,    636,    764,    892,
    1020,  1276,  1532,  1788,  2044,  2556,  3068,  3580,  4092,  5116,  6140,  7164,   8188,   10236,  12284,  14332,
    16380, 20476, 24572, 28668, 32764, 40956, 49148, 57340, 65532, 81916, 98300, 114684, 131068, 163836, 196604, 229372,
};

// The four frequency tables in stream order, so header code can walk them as one sequence of FREQUENCY_COUNT.
typedef struct frequency_tables {
    uint16_t *table[4];
    size_t count[4];
} frequency_tables;

// List a header's frequency tables in stream order (literal lengths, match lengths, distances, literals).
static frequency_tables frequency_tables_of(const block_header *header) {
    // The cast drops `const` so one helper serves reading and writing; callers that only read never write.
    block_header *h = (block_header *)header;
    frequency_tables tables = {
        {h->literal_length_frequencies, h->match_length_frequencies, h->distance_frequencies, h->literal_frequencies},
        {LITERAL_LENGTH_SYMBOLS, MATCH_LENGTH_SYMBOLS, DISTANCE_SYMBOLS, LITERAL_SYMBOLS},
    };
    return tables;
}

// Sum of `count` frequencies.
static uint32_t sum_frequencies(const uint16_t *frequencies, size_t count) {
    uint32_t sum = 0;
    for (size_t i = 0; i < count; i++) sum += frequencies[i];
    return sum;
}

lzfse_zazzcat_status block_header_validate(const block_header *h) {
    bool ok = h->literal_count <= LITERALS_PER_BLOCK && h->token_count <= TOKENS_PER_BLOCK &&
              h->literal_length_state < LITERAL_LENGTH_STATES && h->match_length_state < MATCH_LENGTH_STATES &&
              h->distance_state < DISTANCE_STATES && h->literal_padding_bits >= -7 && h->literal_padding_bits <= 0 &&
              h->token_padding_bits >= -7 && h->token_padding_bits <= 0 &&
              sum_frequencies(h->literal_length_frequencies, LITERAL_LENGTH_SYMBOLS) <= LITERAL_LENGTH_STATES &&
              sum_frequencies(h->match_length_frequencies, MATCH_LENGTH_SYMBOLS) <= MATCH_LENGTH_STATES &&
              sum_frequencies(h->distance_frequencies, DISTANCE_SYMBOLS) <= DISTANCE_STATES &&
              sum_frequencies(h->literal_frequencies, LITERAL_SYMBOLS) <= LITERAL_STATES;
    for (int i = 0; i < 4; i++) ok = ok && h->literal_states[i] < LITERAL_STATES;
    return ok ? LZFSE_ZAZZCAT_OK : LZFSE_ZAZZCAT_CORRUPT;
}

lzfse_zazzcat_status parse_v1_header(const uint8_t *source, size_t source_length, block_header *h,
                                     size_t *header_size) {
    if (source_length < V1_HEADER_SIZE) return LZFSE_ZAZZCAT_TRUNCATED;
    // Not cleared: every field, including all four frequency tables, is read from the header below. Callers use
    // `h` only when this returns OK.
    // clear_bytes(h, sizeof *h);
    h->decoded_size = load_u32(source + 4);
    uint32_t payload_size = load_u32(source + 8);
    h->literal_count = load_u32(source + 12);
    h->token_count = load_u32(source + 16);
    h->literal_payload_size = load_u32(source + 20);
    h->token_payload_size = load_u32(source + 24);
    h->literal_padding_bits = (int32_t)load_u32(source + 28);
    for (int i = 0; i < 4; i++) h->literal_states[i] = load_u16(source + 32 + 2 * i);
    h->token_padding_bits = (int32_t)load_u32(source + 40);
    load_u16_x3(source + 44, &h->literal_length_state, &h->match_length_state, &h->distance_state);
    frequency_tables tables = frequency_tables_of(h);
    size_t offset = 50;
    for (int t = 0; t < 4; t++) {
        for (size_t i = 0; i < tables.count[t]; i++, offset += 2) tables.table[t][i] = load_u16(source + offset);
    }
    // The total payload size must equal the sum of the two parts (checked in 64 bits so it cannot overflow).
    if ((uint64_t)h->literal_payload_size + h->token_payload_size != payload_size) return LZFSE_ZAZZCAT_CORRUPT;
    *header_size = V1_HEADER_SIZE;
    return LZFSE_ZAZZCAT_OK;
}

// Return `width` bits of `word` starting at bit `offset`.
static uint32_t bit_field(uint64_t word, unsigned offset, unsigned width) {
    return (uint32_t)((word >> offset) & ((1ull << width) - 1));
}

uint16_t read_frequency(uint32_t bits, uint32_t *bit_count) {
    if ((bits & 3) == 0) {
        *bit_count = 2;
        return 0;
    }
    if ((bits & 3) == 2) {
        *bit_count = 2;
        return 1;
    }
    if ((bits & 7) == 1) {
        *bit_count = 3;
        return 2;
    }
    if ((bits & 7) == 5) {
        *bit_count = 3;
        return 3;
    }
    if ((bits & 15) == 7) {
        *bit_count = 8;
        return (uint16_t)(8 + ((bits >> 4) & 0xF));
    }
    if ((bits & 15) == 15) {
        *bit_count = 14;
        return (uint16_t)(24 + ((bits >> 4) & 0x3FF));
    }
    *bit_count = 5;
    return (uint16_t)(4 + ((bits >> 3) & 3));
}

uint32_t write_frequency(uint32_t value, uint32_t *bit_count) {
    if (value == 0) {
        *bit_count = 2;
        return 0;
    }
    if (value == 1) {
        *bit_count = 2;
        return 2;
    }
    if (value == 2) {
        *bit_count = 3;
        return 1;
    }
    if (value == 3) {
        *bit_count = 3;
        return 5;
    }
    if (value <= 7) {
        *bit_count = 5;
        return 3 | ((value - 4) << 3);
    }
    if (value <= 23) {
        *bit_count = 8;
        return 7 | ((value - 8) << 4);
    }
    *bit_count = 14;
    return 15 | ((value - 24) << 4);
}

lzfse_zazzcat_status parse_v2_header(const uint8_t *source, size_t source_length, block_header *h,
                                     size_t *header_size) {
    if (source_length < V2_FIXED_HEADER_SIZE) return LZFSE_ZAZZCAT_TRUNCATED;
    uint64_t word0 = load_u64(source + 8), word1 = load_u64(source + 16), word2 = load_u64(source + 24);
    size_t size = bit_field(word2, 0, 32);
    if (size < V2_FIXED_HEADER_SIZE) return LZFSE_ZAZZCAT_CORRUPT;
    if (size > source_length) return LZFSE_ZAZZCAT_TRUNCATED;
    // Not cleared: every scalar field is read from the fixed header below, and the frequency loop sets every
    // frequency. Only a header without tables leaves the frequencies unset, so that case zeroes them itself.
    // Callers use `h` only when this returns OK.
    // clear_bytes(h, sizeof *h);
    h->decoded_size = load_u32(source + 4);
    h->literal_count = bit_field(word0, 0, 20);
    h->literal_payload_size = bit_field(word0, 20, 20);
    h->token_count = bit_field(word0, 40, 20);
    h->literal_padding_bits = (int32_t)bit_field(word0, 60, 3) - 7;
    for (int i = 0; i < 4; i++) h->literal_states[i] = (uint16_t)bit_field(word1, 10 * i, 10);
    h->token_payload_size = bit_field(word1, 40, 20);
    h->token_padding_bits = (int32_t)bit_field(word1, 60, 3) - 7;
    h->literal_length_state = (uint16_t)bit_field(word2, 32, 10);
    h->match_length_state = (uint16_t)bit_field(word2, 42, 10);
    h->distance_state = (uint16_t)bit_field(word2, 52, 10);
    *header_size = size;
    // A header without tables leaves every frequency at zero.
    const uint8_t *tables_start = source + V2_FIXED_HEADER_SIZE;
    size_t tables_length = size - V2_FIXED_HEADER_SIZE;
    frequency_tables tables = frequency_tables_of(h);
    if (tables_length == 0) {
        for (int t = 0; t < 4; t++) clear_bytes(tables.table[t], tables.count[t] * sizeof *tables.table[t]);
        return LZFSE_ZAZZCAT_OK;
    }
    // Read the variable-length frequency codes through a 32-bit accumulator, least significant bits first.
    uint32_t accumulator = 0, available_bits = 0;
    size_t next_byte = 0;
    for (int t = 0; t < 4; t++) {
        for (size_t i = 0; i < tables.count[t]; i++) {
            while (next_byte < tables_length && available_bits + 8 <= 32) {
                accumulator |= (uint32_t)tables_start[next_byte++] << available_bits;
                available_bits += 8;
            }
            uint32_t used;
            tables.table[t][i] = read_frequency(accumulator, &used);
            if (used > available_bits) return LZFSE_ZAZZCAT_CORRUPT;
            accumulator >>= used;
            available_bits -= used;
        }
    }
    // The tables must end within the last byte of the header.
    if (available_bits >= 8 || next_byte != tables_length) return LZFSE_ZAZZCAT_CORRUPT;
    return LZFSE_ZAZZCAT_OK;
}

size_t write_v2_header(const block_header *h, lzfse_zazzcat_buffer *output) {
    // At most 14 bits per frequency, plus the fixed fields.
    if (!buffer_reserve(output, V2_FIXED_HEADER_SIZE + FREQUENCY_COUNT * 14 / 8 + 2)) return 0;
    size_t start = output->length;
    // Not cleared: this placeholder is reserved space, and write_v2_fixed_fields below overwrites all 32 bytes
    // (magic, decoded size and three 64-bit words) before the header is returned.
    // clear_bytes(output->data + start, V2_FIXED_HEADER_SIZE);
    output->length += V2_FIXED_HEADER_SIZE;
    uint32_t accumulator = 0, pending_bits = 0;
    frequency_tables tables = frequency_tables_of(h);
    for (int t = 0; t < 4; t++) {
        for (size_t i = 0; i < tables.count[t]; i++) {
            uint32_t bit_count;
            accumulator |= write_frequency(tables.table[t][i], &bit_count) << pending_bits;
            pending_bits += bit_count;
            while (pending_bits >= 8) {
                buffer_push_reserved(output, (uint8_t)accumulator);
                accumulator >>= 8;
                pending_bits -= 8;
            }
        }
    }
    if (pending_bits > 0) buffer_push_reserved(output, (uint8_t)accumulator);
    size_t header_size = output->length - start;
    write_v2_fixed_fields(h, (uint32_t)header_size, output->data + start);
    return header_size;
}

// Place `value` (which fits in `width` bits) at bit `offset` of a 64-bit word.
static uint64_t place(uint32_t value, unsigned offset) {
    return (uint64_t)value << offset;
}

void write_v2_fixed_fields(const block_header *h, uint32_t header_size, uint8_t fixed[V2_FIXED_HEADER_SIZE]) {
    uint64_t word0 = place(h->literal_count, 0) | place(h->literal_payload_size, 20) | place(h->token_count, 40) |
                     place((uint32_t)(h->literal_padding_bits + 7), 60);
    uint64_t word1 = place(h->literal_states[0], 0) | place(h->literal_states[1], 10) |
                     place(h->literal_states[2], 20) | place(h->literal_states[3], 30) |
                     place(h->token_payload_size, 40) | place((uint32_t)(h->token_padding_bits + 7), 60);
    uint64_t word2 = place(header_size, 0) | place(h->literal_length_state, 32) | place(h->match_length_state, 42) |
                     place(h->distance_state, 52);
    store_u32_x2(fixed, MAGIC_LZFSE_V2, h->decoded_size);
    store_u64_x3(fixed + 8, word0, word1, word2);
}

bool write_v1_header(const block_header *h, lzfse_zazzcat_buffer *output) {
    if (!buffer_reserve(output, V1_HEADER_SIZE)) return false;
    uint8_t *out = output->data + output->length;
    // Not cleared: the stores below write bytes 0 to 769, and the 2 padding bytes at the end are zeroed last.
    // clear_bytes(out, V1_HEADER_SIZE);
    uint32_t fields[8] = {MAGIC_LZFSE_V1,
                          h->decoded_size,
                          h->literal_payload_size + h->token_payload_size,
                          h->literal_count,
                          h->token_count,
                          h->literal_payload_size,
                          h->token_payload_size,
                          (uint32_t)h->literal_padding_bits};
    for (int i = 0; i < 8; i++) store_u32(out + 4 * i, fields[i]);
    for (int i = 0; i < 4; i++) store_u16(out + 32 + 2 * i, h->literal_states[i]);
    store_u32(out + 40, (uint32_t)h->token_padding_bits);
    store_u16_x3(out + 44, h->literal_length_state, h->match_length_state, h->distance_state);
    frequency_tables tables = frequency_tables_of(h);
    size_t offset = 50;
    for (int t = 0; t < 4; t++) {
        for (size_t i = 0; i < tables.count[t]; i++, offset += 2) store_u16(out + offset, tables.table[t][i]);
    }
    // The header struct's trailing padding (offset 770 here) rounds its size up to a multiple of 4.
    store_u16(out + offset, 0);
    output->length += V1_HEADER_SIZE;
    return true;
}
