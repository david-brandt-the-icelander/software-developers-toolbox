// Pieces shared by the encoder's files: the block writer, which collects tokens and entropy-codes them into
// blocks, and the parse loop, which turns input into tokens using a match finder.
#ifndef LZFSE_ZAZZCAT_ENCODE_INTERNAL_H
#define LZFSE_ZAZZCAT_ENCODE_INTERNAL_H

#include "copy.h"
#include "format.h"

// One token: `literal_length` literal bytes, then `match_length` bytes copied from `distance` back. After the
// block is encoded, `symbols` holds the three FSE symbols of its values.
typedef struct token {
    uint32_t literal_length;
    uint32_t match_length;
    uint32_t distance;
    uint8_t symbols[3];
} token;

// The block being built: its tokens and literals, and where it starts in the input. Reused across blocks and
// calls; the two arrays are allocated the first time an LZFSE block is built.
typedef struct block_writer {
    // Offset in the input where the current block starts, and how many input bytes it covers so far.
    size_t block_start;
    size_t covered_bytes;
    token *tokens;
    size_t token_count;
    // Literal bytes of the block, with COPY_SLACK spare bytes for the wide copies.
    uint8_t *literals;
    size_t literal_count;
    // First error (out of memory) while writing blocks; LZFSE_ZAZZCAT_OK otherwise.
    lzfse_zazzcat_status status;
} block_writer;

// Start a new input whose first block begins at input offset 0, allocating the arrays on first use.
lzfse_zazzcat_status block_writer_reset(block_writer *blocks);

// Release the block writer's arrays.
void block_writer_free(block_writer *blocks);

// Encode the buffered tokens as one block (or as a raw block if that is smaller) and append it to `output`.
// A plain call, although the parse loop makes it only about once per block: marking it COLD and
// PRESERVE_REGISTERS, as decode_token_slow is, made the encoder slower (see
// blogs/2026-11-14-keeping-values-in-registers.md).
void block_writer_flush(block_writer *blocks, const uint8_t *source, lzfse_zazzcat_buffer *output);

// Append one token whose values are in range, closing the block first if it is full. Each field of `blocks` is
// read once into a local: the literal copy stores bytes, and a byte store may change any memory, `*blocks`
// included, so a field read again after it would be loaded again.
ALWAYS_INLINE void block_writer_token(block_writer *blocks, const uint8_t *source, size_t source_length,
                                      size_t literal_start, size_t literal_length, uint32_t length, uint32_t distance,
                                      lzfse_zazzcat_buffer *output) {
    if (blocks->token_count + 1 > TOKENS_PER_BLOCK || blocks->literal_count + literal_length + 3 > LITERALS_PER_BLOCK) {
        block_writer_flush(blocks, source, output);
    }
    size_t token_count = blocks->token_count;
    size_t literal_count = blocks->literal_count;
    size_t covered_bytes = blocks->covered_bytes;
    token *t = &blocks->tokens[token_count];
    uint8_t *literal_destination = blocks->literals + literal_count;
    blocks->token_count = token_count + 1;
    blocks->literal_count = literal_count + literal_length;
    blocks->covered_bytes = covered_bytes + literal_length + length;
    t->literal_length = (uint32_t)literal_length;
    t->match_length = length;
    t->distance = distance;
    // Wide copy of the literals; any overshoot lands in the slack and the next literals overwrite it. Near the end
    // of the input there may be fewer than 16 spare bytes to read, so copy exactly there (inline, without a call).
    if (literal_start + literal_length + 16 <= source_length) {
        wild_copy(literal_destination, source + literal_start, literal_length);
    } else {
        copy_exact_inline(literal_destination, source + literal_start, literal_length);
    }
}

// Add the literals source[literal_start .. literal_end) followed by a match of `length` bytes at `distance`
// (length 0 for the trailing literals), splitting values that exceed the format's limits.
ALWAYS_INLINE void block_writer_push(block_writer *blocks, const uint8_t *source, size_t source_length,
                                     size_t literal_start, size_t literal_end, uint32_t length, uint32_t distance,
                                     lzfse_zazzcat_buffer *output) {
    while (literal_end - literal_start > MAX_LITERAL_LENGTH) {
        block_writer_token(blocks, source, source_length, literal_start, MAX_LITERAL_LENGTH, 0, 0, output);
        literal_start += MAX_LITERAL_LENGTH;
    }
    while (length > MAX_MATCH_LENGTH) {
        block_writer_token(blocks, source, source_length, literal_start, literal_end - literal_start, MAX_MATCH_LENGTH,
                           distance, output);
        literal_start = literal_end;
        length -= MAX_MATCH_LENGTH;
    }
    if (literal_start < literal_end || length > 0) {
        block_writer_token(blocks, source, source_length, literal_start, literal_end - literal_start, length, distance,
                           output);
    }
}

// A candidate match: where it starts, how long it is and how far back it points (length 0 means none).
typedef struct match {
    size_t start;
    size_t length;
    size_t distance;
} match;

// A match finder, as three functions over its own state (`finder`):
//   find:        the best match at `position` (not starting before `literal_start`), also checking
//                `previous_distance`; then index `position`. Returns a match of length 0 when there is none.
//   insert:      index `position` in both of the finder's tables, without searching.
//   insert_long: index `position` in the long (8-byte) table only.
typedef match (*find_function)(void *finder, size_t position, size_t literal_start, size_t previous_distance);
typedef void (*insert_function)(void *finder, size_t position);

// Tokenise `source` with a match finder and write its blocks to `output`: greedy matching, with one step of lazy
// evaluation and a held-back last match.
//
// This function is always inlined, and each caller passes the addresses of specific always-inline functions, so
// the compiler inlines those too: every finder gets its own specialized copy of the loop, with no calls.
ALWAYS_INLINE void parse(const uint8_t *source, size_t source_length, void *finder, find_function find,
                         insert_function insert, insert_function insert_long, block_writer *blocks,
                         const lzfse_zazzcat_options *options, lzfse_zazzcat_buffer *output) {
    // The options as locals: read through `options` they would be loaded again after every table or literal store
    // (which, as far as the compiler knows, might change them).
    uint32_t skip_shift = options->skip_shift;
    size_t lazy_below = options->lazy_below;
    // The finders read 8 bytes at a position, so they stop 8 bytes before the end.
    size_t limit = source_length > 8 ? source_length - 8 : 0;
    size_t position = 0, literal_start = 0, previous_distance = 0, misses = 0;
    // The last match found, held back because the next one may swallow it; its literals start at
    // `literal_start`. A length of 0 means there is none.
    match held = {0, 0, 0};
    while (position < limit) {
        match m = find(finder, position, literal_start, previous_distance);
        if (m.length == 0) {
            misses++;
            // After many misses in a row (incompressible data), step further ahead each time.
            position += skip_shift == 0 ? 1 : 1 + (misses >> skip_shift);
            continue;
        }
        misses = 0;
        if (m.length < lazy_below && position + 1 < limit) {
            match next = find(finder, position + 1, literal_start, previous_distance);
            if (next.length > m.length) m = next;
        }
        // A new match that extends backward over all of the held one replaces it: one token instead of two, as
        // when a short match on a common prefix is followed by a long copy that includes that prefix. Otherwise
        // the held match is emitted, and a new match reaching back into it is cut where it ends.
        if (held.length != 0) {
            size_t held_end = held.start + held.length;
            if (m.start > held.start) {
                if (m.start < held_end) {
                    m.length -= held_end - m.start;
                    m.start = held_end;
                }
                block_writer_push(blocks, source, source_length, literal_start, held.start, (uint32_t)held.length,
                                  (uint32_t)held.distance, output);
                literal_start = held_end;
            }
        }
        held = m;
        previous_distance = m.distance;
        size_t end = m.start + m.length;
        // Index positions inside the match so later data can refer to them. Long matches go into the 4-byte table
        // only at their edges, so they do not flush it. Their interior goes into the long table: when this content
        // repeats later, possibly edited, the copy right after an edit is then found here. Runs and short periods
        // (distance below 16) are skipped, since every position there holds the same few bytes.
        bool index_interior = m.distance >= 16;
        size_t index_end = min_size(end, limit);
        for (size_t j = max_size(position + 2, m.start + 1); j < index_end;) {
            if (m.length > 64 && j > m.start + 16 && j + 16 < end) {
                if (!index_interior) {
                    j = end - 16;
                    continue;
                }
                insert_long(finder, j);
            } else {
                insert(finder, j);
            }
            j++;
        }
        position = end;
    }
    if (held.length != 0) {
        block_writer_push(blocks, source, source_length, literal_start, held.start, (uint32_t)held.length,
                          (uint32_t)held.distance, output);
        literal_start = held.start + held.length;
    }
    block_writer_push(blocks, source, source_length, literal_start, source_length, 0, 0, output);
    block_writer_flush(blocks, source, output);
}

// The 64 KiB fast path's tables (see encode_small.c), kept in the encoder between calls.
typedef struct small_finder_tables {
    struct small_bucket *buckets;
    size_t bucket_capacity;
    uint16_t *long_slots;
    size_t long_capacity;
} small_finder_tables;

// Largest input the fast path handles (positions then fit in 16 bits).
#define SMALL_INPUT_MAX 65536

// Tokenise `source` (4 KiB to 64 KiB) with the fast-path finder and write its blocks to `output`.
lzfse_zazzcat_status encode_small(const uint8_t *source, size_t source_length, small_finder_tables *tables,
                                  block_writer *blocks, const lzfse_zazzcat_options *options,
                                  lzfse_zazzcat_buffer *output);

// Release the fast path's tables.
void small_finder_tables_free(small_finder_tables *tables);

#endif
