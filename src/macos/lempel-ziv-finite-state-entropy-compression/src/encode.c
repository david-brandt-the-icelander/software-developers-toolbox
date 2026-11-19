// LZFSE stream encoder: chooses the block type, finds matches in inputs over 64 KiB, and hands tokens to the
// block writer. Inputs of 64 KiB or less use the fast path in encode_small.c.
#include "buffer.h"
#include "encode_internal.h"
#include "lzvn.h"

// Positions are stored as 32-bit numbers, so very large inputs are cut into independent segments of this size.
#define SEGMENT_SIZE ((size_t)1 << 30)
// Candidates kept per hash bucket of the 4-byte table.
#define BUCKET_WAYS 4
// The long (8-byte) table has 2^LONG_TABLE_EXTRA_BITS times as many slots as the 4-byte table has buckets.
#define LONG_TABLE_EXTRA_BITS 2
// Allowed range of the hash_bits option (it sets table sizes, which must stay sensible).
#define MIN_HASH_BITS 10
#define MAX_HASH_BITS 22

// One bucket of the 4-byte table: the most recent positions whose 4 bytes hash here (newest first), stored as
// `position_base + position`, and those 4 bytes themselves, which reject most false candidates with one compare.
// Each array of four 32-bit entries is packed two per 64-bit word, newest in the low half of the first word, so
// recording a position is a few shifts on four words (bucket_shift_in) instead of moving eight entries. That
// saved 1-2% of the encoder's instructions for large inputs.
typedef struct hash_bucket {
    uint64_t positions[2];
    uint64_t values[2];
} hash_bucket;

// Entry `k` (0 = newest) of a packed pair of words: word k / 2 (k >> 1), shifted down by 32 bits when k is odd.
ALWAYS_INLINE uint32_t bucket_lane(const uint64_t words[2], int k) {
    return (uint32_t)(words[k >> 1] >> (32 * (k & 1)));
}

// Shift a packed pair of words by one lane, dropping the oldest entry, and put `newest` in lane 0.
ALWAYS_INLINE void bucket_shift_in(uint64_t words[2], uint32_t newest) {
    words[1] = (words[1] << 32) | (words[0] >> 32);
    words[0] = (words[0] << 32) | newest;
}

struct lzfse_zazzcat_encoder {
    lzfse_zazzcat_options options;
    // The large-input match finder's tables: 4-way buckets keyed on 4 bytes, and single positions keyed on 8
    // bytes. Common short strings (frequent words, JSON keys) refill the 4-byte buckets constantly; the long table
    // keeps the starts of long repeats that those would push out.
    hash_bucket *buckets;
    size_t bucket_count;
    uint32_t *long_slots;
    // Added to positions before they are stored. Each input moves it past everything stored so far, so entries
    // from earlier inputs are recognizably stale and the tables never need clearing between calls.
    uint32_t position_base;
    block_writer blocks;
    lzvn_hash_table lzvn_table;
    small_finder_tables small_tables;
};

lzfse_zazzcat_options lzfse_zazzcat_default_options(void) {
    lzfse_zazzcat_options options = {14, 40, 8, 6};
    return options;
}

void lzfse_zazzcat_encoder_set_options(lzfse_zazzcat_encoder *encoder, const lzfse_zazzcat_options *options) {
    encoder->options = options != NULL ? *options : lzfse_zazzcat_default_options();
    if (encoder->options.hash_bits < MIN_HASH_BITS) encoder->options.hash_bits = MIN_HASH_BITS;
    if (encoder->options.hash_bits > MAX_HASH_BITS) encoder->options.hash_bits = MAX_HASH_BITS;
}

lzfse_zazzcat_encoder *lzfse_zazzcat_encoder_create(const lzfse_zazzcat_options *options) {
    // calloc zeroes the struct, so every pointer starts NULL and every table is allocated on first use.
    lzfse_zazzcat_encoder *encoder = zz_calloc(1, sizeof *encoder);
    if (encoder == NULL) return NULL;
    lzfse_zazzcat_encoder_set_options(encoder, options);
    encoder->position_base = 1;
    return encoder;
}

void lzfse_zazzcat_encoder_destroy(lzfse_zazzcat_encoder *encoder) {
    if (encoder == NULL) return;
    zz_free(encoder->buckets);
    zz_free(encoder->long_slots);
    block_writer_free(&encoder->blocks);
    zz_free(encoder->lzvn_table.entries);
    small_finder_tables_free(&encoder->small_tables);
    zz_free(encoder);
}

// Append an uncompressed (`bvx-`) block holding `source`, unless it is empty.
static lzfse_zazzcat_status push_uncompressed(lzfse_zazzcat_buffer *output, const uint8_t *source, size_t length) {
    if (length == 0) return LZFSE_ZAZZCAT_OK;
    if (!buffer_reserve(output, UNCOMPRESSED_HEADER_SIZE + length)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    buffer_push_u32_x2_reserved(output, MAGIC_UNCOMPRESSED, (uint32_t)length);
    buffer_append_reserved(output, source, length);
    return LZFSE_ZAZZCAT_OK;
}

// --- The large-input match finder ---

// The finder's view of the encoder's tables for one segment.
typedef struct large_finder {
    const uint8_t *source;
    size_t source_length;
    hash_bucket *buckets;
    uint32_t *long_slots;
    uint32_t bucket_shift;
    uint32_t long_shift;
    size_t good_match;
    uint32_t position_base;
} large_finder;

// Hash 4 bytes into a bucket index (multiplicative hashing: the top bits of the product).
ALWAYS_INLINE size_t bucket_index(const large_finder *f, uint32_t four_bytes) {
    return (size_t)((four_bytes * 0x9E3779B1u) >> f->bucket_shift);
}

// Hash 8 bytes into a long-table slot.
ALWAYS_INLINE size_t long_slot_index(const large_finder *f, uint64_t eight_bytes) {
    return (size_t)((eight_bytes * 0x9E3779B97F4A7C15ull) >> f->long_shift);
}

// Record `position` (whose first 4 bytes are `four_bytes`) in its bucket, evicting the oldest entry.
ALWAYS_INLINE void large_insert_short(large_finder *f, size_t position, uint32_t four_bytes) {
    hash_bucket *b = &f->buckets[bucket_index(f, four_bytes)];
    bucket_shift_in(b->positions, f->position_base + (uint32_t)position);
    bucket_shift_in(b->values, four_bytes);
}

// Record `position` in both tables.
ALWAYS_INLINE void large_insert(void *finder, size_t position) {
    large_finder *f = finder;
    uint64_t eight = load_u64(f->source + position);
    large_insert_short(f, position, (uint32_t)eight);
    f->long_slots[long_slot_index(f, eight)] = f->position_base + (uint32_t)position;
}

// Record `position` in the long table only.
ALWAYS_INLINE void large_insert_long(void *finder, size_t position) {
    large_finder *f = finder;
    f->long_slots[long_slot_index(f, load_u64(f->source + position))] = f->position_base + (uint32_t)position;
}

// Find the best match at `position` among the previous distance, the long table and the bucket; then record
// `position`. Stored positions that are stale (from earlier inputs) or empty are further back than `position`
// and fail the distance check.
ALWAYS_INLINE match large_find(void *finder, size_t position, size_t literal_start, size_t previous_distance) {
    large_finder *f = finder;
    const uint8_t *src = f->source;
    size_t n = f->source_length;
    uint64_t eight = load_u64(src + position);
    uint32_t four = (uint32_t)eight;
    hash_bucket bucket = f->buckets[bucket_index(f, four)];
    size_t slot = long_slot_index(f, eight);
    uint32_t long_candidate = f->long_slots[slot];
    match best = {position, 0, 0};
    if (previous_distance != 0 && previous_distance <= position &&
        load_u32(src + position - previous_distance) == four) {
        best.length = 4 + match_length(src, position - previous_distance + 4, position + 4, n);
        best.distance = previous_distance;
    }
    uint32_t here = f->position_base + (uint32_t)position;
    f->long_slots[slot] = here;
    // Unsigned subtraction: a stale or empty slot gives a huge distance, rejected below.
    size_t distance = (uint32_t)(here - long_candidate);
    if (best.length < f->good_match && distance != 0 && distance <= position && distance <= MAX_DISTANCE &&
        load_u64(src + position - distance) == eight) {
        size_t length = 8 + match_length(src, position - distance + 8, position + 8, n);
        if (length > best.length) best = (match){position, length, distance};
    }
    for (int k = 0; k < BUCKET_WAYS; k++) {
        if (best.length >= f->good_match) break;
        if (bucket_lane(bucket.values, k) != four) continue;
        distance = (uint32_t)(here - bucket_lane(bucket.positions, k));
        if (distance == 0 || distance > position || distance > MAX_DISTANCE) continue;
        size_t length = 4 + match_length(src, position - distance + 4, position + 4, n);
        if (length > best.length) best = (match){position, length, distance};
    }
    large_insert_short(f, position, four);
    if (best.length < 4) return (match){position, 0, 0};
    // Extend the match backward over pending literals.
    size_t back = match_length_backward(src, position - best.distance, position, position - literal_start);
    best.start -= back;
    best.length += back;
    return best;
}

// Make the tables ready for an input of `length` bytes and return the hash bits to use. The tables are allocated
// once and cleared only when the position base would overflow; inputs smaller than the tables use part of them.
static lzfse_zazzcat_status prepare_large_tables(lzfse_zazzcat_encoder *e, size_t length, uint32_t *bits_out) {
    uint32_t fit = (uint32_t)(64 - __builtin_clzll((unsigned long long)length));
    fit = fit >= 2 ? fit - 2 : 0;
    if (fit < MIN_HASH_BITS) fit = MIN_HASH_BITS;
    uint32_t bits = e->options.hash_bits < fit ? e->options.hash_bits : fit;
    if (e->bucket_count < ((size_t)1 << bits) || (uint64_t)e->position_base + length + 2 > UINT32_MAX) {
        size_t count = max_size((size_t)1 << bits, e->bucket_count);
        zz_free(e->buckets);
        zz_free(e->long_slots);
        e->buckets = zz_calloc(count, sizeof(hash_bucket));
        e->long_slots = zz_calloc(count << LONG_TABLE_EXTRA_BITS, sizeof(uint32_t));
        if (e->buckets == NULL || e->long_slots == NULL) {
            zz_free(e->buckets);
            zz_free(e->long_slots);
            e->buckets = NULL;
            e->long_slots = NULL;
            e->bucket_count = 0;
            return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
        }
        e->bucket_count = count;
        e->position_base = 1;
    }
    *bits_out = bits;
    return LZFSE_ZAZZCAT_OK;
}

// Tokenise one segment and append its blocks to `output`.
static lzfse_zazzcat_status encode_segment(lzfse_zazzcat_encoder *e, const uint8_t *source, size_t length,
                                           lzfse_zazzcat_buffer *output) {
    lzfse_zazzcat_status status = block_writer_reset(&e->blocks);
    if (status != LZFSE_ZAZZCAT_OK) return status;
    if (length <= SMALL_INPUT_MAX) {
        return encode_small(source, length, &e->small_tables, &e->blocks, &e->options, output);
    }
    uint32_t bits;
    if ((status = prepare_large_tables(e, length, &bits))) return status;
    large_finder finder = {
        source,
        length,
        e->buckets,
        e->long_slots,
        32 - bits,
        64 - bits - LONG_TABLE_EXTRA_BITS,
        e->options.good_match,
        e->position_base,
    };
    parse(source, length, &finder, large_find, large_insert, large_insert_long, &e->blocks, &e->options, output);
    // The next input's positions start past everything stored now, so none of these entries look valid then.
    e->position_base += (uint32_t)length + 1;
    return e->blocks.status;
}

lzfse_zazzcat_status lzfse_zazzcat_encoder_encode(lzfse_zazzcat_encoder *e, const void *source_bytes, size_t length,
                                                  lzfse_zazzcat_buffer *output) {
    const uint8_t *source = source_bytes;
    lzfse_zazzcat_status status = LZFSE_ZAZZCAT_OK;
    size_t mark = output->length;
    if (length < 8) {
        status = push_uncompressed(output, source, length);
    } else if (length < LZFSE_ZAZZCAT_LZVN_THRESHOLD) {
        // Small inputs: one LZVN block, or a raw block if LZVN does not make it smaller.
        if (!buffer_reserve(output, LZVN_HEADER_SIZE)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
        // Not cleared: this placeholder is reserved space. On success the store_u32_x3 call below overwrites
        // all 12 bytes; otherwise output->length goes back to `mark` and the placeholder is dropped.
        // clear_bytes(output->data + mark, LZVN_HEADER_SIZE);
        output->length += LZVN_HEADER_SIZE;
        status = lzvn_encode_with_table(source, length, output, &e->lzvn_table);
        size_t payload = output->length - mark - LZVN_HEADER_SIZE;
        if (status == LZFSE_ZAZZCAT_OK && payload < length) {
            store_u32_x3(output->data + mark, MAGIC_LZVN, (uint32_t)length, (uint32_t)payload);
        } else if (status == LZFSE_ZAZZCAT_OK) {
            output->length = mark;
            status = push_uncompressed(output, source, length);
        }
    } else {
        for (size_t at = 0; at < length && status == LZFSE_ZAZZCAT_OK; at += SEGMENT_SIZE) {
            status = encode_segment(e, source + at, min_size(SEGMENT_SIZE, length - at), output);
        }
    }
    if (status == LZFSE_ZAZZCAT_OK && !buffer_reserve(output, 4)) status = LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    if (status != LZFSE_ZAZZCAT_OK) {
        output->length = mark;
        return status;
    }
    buffer_push_u32_reserved(output, MAGIC_END_OF_STREAM);
    return LZFSE_ZAZZCAT_OK;
}

lzfse_zazzcat_status lzfse_zazzcat_encode(const void *source, size_t length, const lzfse_zazzcat_options *options,
                                          lzfse_zazzcat_buffer *output) {
    lzfse_zazzcat_encoder *encoder = lzfse_zazzcat_encoder_create(options);
    if (encoder == NULL) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    // Reserve the output up front. Inputs below the LZVN threshold get everything the LZVN path will reserve
    // (header, payload bound, end-of-stream marker), so it does not move the buffer straight away; larger inputs
    // get about half their size, as most inputs compress at least that well.
    size_t initial =
        length < LZFSE_ZAZZCAT_LZVN_THRESHOLD ? LZVN_HEADER_SIZE + lzvn_encode_bound(length) + 4 : length / 2 + 64;
    if (!buffer_reserve(output, initial)) {
        lzfse_zazzcat_encoder_destroy(encoder);
        return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    }
    lzfse_zazzcat_status status = lzfse_zazzcat_encoder_encode(encoder, source, length, output);
    lzfse_zazzcat_encoder_destroy(encoder);
    return status;
}
