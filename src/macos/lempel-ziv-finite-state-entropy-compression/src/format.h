// The LZFSE stream format: block magics, limits, value tables and block headers.
//
// A stream is a sequence of blocks, each starting with a 4-byte magic, and ends with the end-of-stream magic.
// A compressed (bvx2) block holds tokens: each token is L literal bytes followed by a match of M bytes copied from
// D bytes back. The literals and the (L, M, D) values are entropy-coded with FSE (see fse.h).
#ifndef LZFSE_ZAZZCAT_FORMAT_H
#define LZFSE_ZAZZCAT_FORMAT_H

#include "common.h"

// Block magics, as little-endian integers of their 4 ASCII bytes.
#define MAGIC_END_OF_STREAM 0x24787662u // "bvx$"
#define MAGIC_UNCOMPRESSED  0x2d787662u // "bvx-": a raw copy of the data
#define MAGIC_LZFSE_V1      0x31787662u // "bvx1": LZFSE block with an uncompressed header
#define MAGIC_LZFSE_V2      0x32787662u // "bvx2": LZFSE block with a packed header
#define MAGIC_LZVN          0x6e787662u // "bvxn": LZVN block

// Number of symbols in each FSE alphabet.
#define LITERAL_LENGTH_SYMBOLS 20
#define MATCH_LENGTH_SYMBOLS   20
#define DISTANCE_SYMBOLS       64
#define LITERAL_SYMBOLS        256
#define FREQUENCY_COUNT        (LITERAL_LENGTH_SYMBOLS + MATCH_LENGTH_SYMBOLS + DISTANCE_SYMBOLS + LITERAL_SYMBOLS)

// Number of FSE states (table size) of each alphabet.
#define LITERAL_LENGTH_STATES 64u
#define MATCH_LENGTH_STATES   64u
#define DISTANCE_STATES       256u
#define LITERAL_STATES        1024u

// Block and value limits.
#define TOKENS_PER_BLOCK   10000
#define LITERALS_PER_BLOCK (4 * TOKENS_PER_BLOCK)
#define MAX_LITERAL_LENGTH 315u
#define MAX_MATCH_LENGTH   2359u
#define MAX_DISTANCE       262139u

// Header sizes.
#define V1_HEADER_SIZE           772
#define V2_FIXED_HEADER_SIZE     32
#define UNCOMPRESSED_HEADER_SIZE 8
#define LZVN_HEADER_SIZE         12

// A value of an alphabet is coded as a symbol plus `extra_bits[symbol]` raw bits added to `base[symbol]`.
extern const uint8_t literal_length_extra_bits[LITERAL_LENGTH_SYMBOLS];
extern const uint32_t literal_length_base[LITERAL_LENGTH_SYMBOLS];
extern const uint8_t match_length_extra_bits[MATCH_LENGTH_SYMBOLS];
extern const uint32_t match_length_base[MATCH_LENGTH_SYMBOLS];
extern const uint8_t distance_extra_bits[DISTANCE_SYMBOLS];
extern const uint32_t distance_base[DISTANCE_SYMBOLS];

// Symbol of a literal length (at most MAX_LITERAL_LENGTH): 0-15 code themselves, larger ones share symbols 16-19.
ALWAYS_INLINE uint32_t literal_length_symbol(uint32_t length) {
    if (length < 16) return length;
    return 16 + (length >= 20) + (length >= 28) + (length >= 60);
}

// Symbol of a match length (at most MAX_MATCH_LENGTH).
ALWAYS_INLINE uint32_t match_length_symbol(uint32_t length) {
    if (length < 16) return length;
    return 16 + (length >= 24) + (length >= 56) + (length >= 312);
}

// Symbol of a distance (at most MAX_DISTANCE): groups of 4 symbols share an extra-bit count k, with bases
// 2^(k+2) - 4 + j * 2^k.
ALWAYS_INLINE uint32_t distance_symbol(uint32_t distance) {
    if (distance < 4) return distance;
    // __builtin_clz counts leading zero bits, so 31 - clz(x) is the index of x's highest set bit.
    uint32_t k = (31 - (uint32_t)__builtin_clz(distance + 4)) - 2;
    return 4 * k + ((distance + 4 - (1u << (k + 2))) >> k);
}

// The fields of an LZFSE block header (v1 and v2 headers both decode to this).
typedef struct block_header {
    uint32_t decoded_size;
    uint32_t literal_count;
    uint32_t token_count;
    // Bytes of the literal stream and of the token (L, M, D) stream that follow the header.
    uint32_t literal_payload_size;
    uint32_t token_payload_size;
    // Padding bits at the end of each bit stream, as a count in [-7, 0].
    int32_t literal_padding_bits;
    int32_t token_padding_bits;
    // FSE states the decoder starts from (four interleaved literal states).
    uint16_t literal_states[4];
    uint16_t literal_length_state;
    uint16_t match_length_state;
    uint16_t distance_state;
    // Normalized symbol frequencies; each table sums to at most its state count.
    uint16_t literal_length_frequencies[LITERAL_LENGTH_SYMBOLS];
    uint16_t match_length_frequencies[MATCH_LENGTH_SYMBOLS];
    uint16_t distance_frequencies[DISTANCE_SYMBOLS];
    uint16_t literal_frequencies[LITERAL_SYMBOLS];
} block_header;

// Read a little-endian 32-bit integer at `offset`, or report truncation.
ALWAYS_INLINE lzfse_zazzcat_status read_u32_at(const uint8_t *source, size_t source_length, size_t offset,
                                               uint32_t *value) {
    if (offset > source_length || source_length - offset < 4) return LZFSE_ZAZZCAT_TRUNCATED;
    *value = load_u32(source + offset);
    return LZFSE_ZAZZCAT_OK;
}

// Decode one frequency from the low bits of `bits` (a variable-length code of 2 to 14 bits); store the number of
// bits it used in `*bit_count`.
uint16_t read_frequency(uint32_t bits, uint32_t *bit_count);

// Encode one frequency (the inverse of read_frequency): return the bits and store their count in `*bit_count`.
// `value` is a 16-bit frequency, taken as uint32_t so the arithmetic below stays unsigned.
uint32_t write_frequency(uint32_t value, uint32_t *bit_count);

// Check the limits the decoder relies on: counts, starting states, padding and table sums.
lzfse_zazzcat_status block_header_validate(const block_header *header);

// Parse a v1 header from `source` (which starts at the block magic) and store its size in `*header_size`.
lzfse_zazzcat_status parse_v1_header(const uint8_t *source, size_t source_length, block_header *header,
                                     size_t *header_size);

// Parse a v2 header from `source` (which starts at the block magic) and store its size in `*header_size`.
lzfse_zazzcat_status parse_v2_header(const uint8_t *source, size_t source_length, block_header *header,
                                     size_t *header_size);

// Append a v2 header: 32 fixed bytes (written with placeholder sizes) followed by the compressed frequency
// tables. Returns the header size, or 0 when memory runs out.
size_t write_v2_header(const block_header *header, lzfse_zazzcat_buffer *output);

// Fill in the 32 fixed bytes of a v2 header of `header_size` bytes (magic, decoded size and the packed fields).
void write_v2_fixed_fields(const block_header *header, uint32_t header_size, uint8_t fixed[V2_FIXED_HEADER_SIZE]);

// Append a v1 header (Apple's encoder never writes v1; the tests use it to exercise the decoder).
bool write_v1_header(const block_header *header, lzfse_zazzcat_buffer *output);

#endif
