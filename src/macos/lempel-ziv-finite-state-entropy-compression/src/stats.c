// Stream statistics for diagnostics: totals and token distributions read from block headers and match streams,
// without decoding any output. The benchmark uses them to compare what two encoders chose on the same input.
#include "bare_functions.h"
#include "bits.h"
#include "format.h"
#include "fse.h"

const uint32_t lzfse_zazzcat_length_bucket_limits[LZFSE_ZAZZCAT_LENGTH_BUCKETS] = {8,   16,   32,   64,
                                                                                   256, 1024, 4096, UINT32_MAX};
const uint32_t lzfse_zazzcat_distance_bucket_limits[LZFSE_ZAZZCAT_DISTANCE_BUCKETS] = {16,    256,    4096,
                                                                                       65536, 131072, UINT32_MAX};

// Index of the first bucket whose limit is above `value` (the last bucket catches everything else).
static size_t bucket_of(const uint32_t *limits, size_t count, uint32_t value) {
    for (size_t i = 0; i < count; i++) {
        if (value < limits[i]) return i;
    }
    return count - 1;
}

lzfse_zazzcat_status lzfse_zazzcat_stream_stats_add(lzfse_zazzcat_stream_stats *s, const void *stream_bytes,
                                                    size_t length) {
    const uint8_t *src = stream_bytes;
    // The decoding tables are large, so they live on the heap for the duration of the call.
    fse_value_entry *tables =
        zz_malloc((LITERAL_LENGTH_STATES + MATCH_LENGTH_STATES + DISTANCE_STATES) * sizeof(fse_value_entry));
    if (tables == NULL) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    fse_value_entry *l_table = tables, *m_table = l_table + LITERAL_LENGTH_STATES;
    fse_value_entry *d_table = m_table + MATCH_LENGTH_STATES;
    lzfse_zazzcat_status status = LZFSE_ZAZZCAT_OK;
    size_t offset = 0;
    for (;;) {
        uint32_t magic, value;
        if ((status = read_u32_at(src, length, offset, &magic))) break;
        if (magic == MAGIC_END_OF_STREAM) {
            s->stored_bytes += offset + 4;
            break;
        } else if (magic == MAGIC_UNCOMPRESSED) {
            if ((status = read_u32_at(src, length, offset + 4, &value))) break;
            s->blocks[2]++;
            s->decoded_bytes += value;
            s->other_block_bytes += UNCOMPRESSED_HEADER_SIZE + (uint64_t)value;
            offset += UNCOMPRESSED_HEADER_SIZE + (size_t)value;
        } else if (magic == MAGIC_LZVN) {
            uint32_t payload;
            if ((status = read_u32_at(src, length, offset + 4, &value)) ||
                (status = read_u32_at(src, length, offset + 8, &payload))) {
                break;
            }
            s->blocks[1]++;
            s->decoded_bytes += value;
            s->other_block_bytes += LZVN_HEADER_SIZE + (uint64_t)payload;
            offset += LZVN_HEADER_SIZE + (size_t)payload;
        } else if (magic == MAGIC_LZFSE_V1 || magic == MAGIC_LZFSE_V2) {
            block_header h;
            size_t header_size;
            status = magic == MAGIC_LZFSE_V1 ? parse_v1_header(src + offset, length - offset, &h, &header_size)
                                             : parse_v2_header(src + offset, length - offset, &h, &header_size);
            if (status || (status = block_header_validate(&h))) break;
            size_t token_start = offset + header_size + h.literal_payload_size;
            size_t token_end = token_start + h.token_payload_size;
            if (token_end > length) {
                status = LZFSE_ZAZZCAT_TRUNCATED;
                break;
            }
            s->blocks[0]++;
            s->decoded_bytes += h.decoded_size;
            s->header_bytes += header_size;
            s->literal_bytes += h.literal_payload_size;
            s->match_stream_bytes += h.token_payload_size;
            s->literals += h.literal_count;
            s->tokens += h.token_count;
            if ((status = fse_value_table(h.literal_length_frequencies, LITERAL_LENGTH_SYMBOLS, LITERAL_LENGTH_STATES,
                                          literal_length_extra_bits, literal_length_base, l_table)) ||
                (status = fse_value_table(h.match_length_frequencies, MATCH_LENGTH_SYMBOLS, MATCH_LENGTH_STATES,
                                          match_length_extra_bits, match_length_base, m_table)) ||
                (status = fse_value_table(h.distance_frequencies, DISTANCE_SYMBOLS, DISTANCE_STATES,
                                          distance_extra_bits, distance_base, d_table))) {
                break;
            }
            bit_reader reader; // bit_reader_init sets every field when it succeeds
            if ((status = bit_reader_init(&reader, src + token_start, h.token_payload_size, h.token_padding_bits))) {
                break;
            }
            uint32_t l_state = h.literal_length_state, m_state = h.match_length_state, d_state = h.distance_state;
            uint32_t previous_distance = 0;
            for (uint32_t t = 0; t < h.token_count; t++) {
                if ((status = bit_reader_refill(&reader))) break;
                bare_fse_decode_value(&l_state, l_table, LITERAL_LENGTH_STATES - 1, &reader);
                uint32_t match = fse_decode_value(&m_state, m_table, MATCH_LENGTH_STATES - 1, &reader);
                uint32_t coded = fse_decode_value(&d_state, d_table, DISTANCE_STATES - 1, &reader);
                // Literal-only tokens carry a distance too, and it becomes the previous distance.
                uint32_t distance = coded != 0 ? coded : previous_distance;
                previous_distance = distance;
                if (match == 0) {
                    s->literal_only_tokens++;
                    continue;
                }
                if (coded == 0) s->repeat_distances++;
                size_t lb = bucket_of(lzfse_zazzcat_length_bucket_limits, LZFSE_ZAZZCAT_LENGTH_BUCKETS, match);
                size_t db = bucket_of(lzfse_zazzcat_distance_bucket_limits, LZFSE_ZAZZCAT_DISTANCE_BUCKETS, distance);
                s->matches_by_length[lb]++;
                s->bytes_by_length[lb] += match;
                s->matches_by_distance[db]++;
                s->bytes_by_distance[db] += match;
            }
            if (status) break;
            offset = token_end;
        } else {
            status = LZFSE_ZAZZCAT_CORRUPT;
            break;
        }
    }
    zz_free(tables);
    return status;
}
