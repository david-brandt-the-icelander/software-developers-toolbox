// The block writer: turns a block's buffered tokens and literals into an entropy-coded `bvx2` block, or stores
// the block's input raw (`bvx-`) if that is smaller.
#include "bits.h"
#include "encode_internal.h"
#include "fse.h"

lzfse_zazzcat_status block_writer_reset(block_writer *blocks) {
    blocks->block_start = 0;
    blocks->covered_bytes = 0;
    blocks->token_count = 0;
    blocks->literal_count = 0;
    blocks->status = LZFSE_ZAZZCAT_OK;
    if (blocks->tokens == NULL) {
        blocks->tokens = zz_malloc(TOKENS_PER_BLOCK * sizeof(token));
        if (blocks->tokens == NULL) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    }
    if (blocks->literals == NULL) {
        blocks->literals = zz_malloc(LITERALS_PER_BLOCK + COPY_SLACK);
        if (blocks->literals == NULL) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    }
    return LZFSE_ZAZZCAT_OK;
}

void block_writer_free(block_writer *blocks) {
    zz_free(blocks->tokens);
    zz_free(blocks->literals);
    blocks->tokens = NULL;
    blocks->literals = NULL;
}

// Count how often each byte value occurs in `literals[0 .. count)` (`count` a multiple of 4) into `counts`.
// Each step loads 4 literals at once, and each of the 4 byte lanes has its own table of counts. With one table, a
// run of equal bytes makes every step wait for the previous step's store to the same counter; with four, the lanes
// update different counters and the steps overlap. The tables are added together at the end.
static void count_literals(const uint8_t *literals, size_t count, uint32_t counts[LITERAL_SYMBOLS]) {
    uint32_t lane_counts[4][LITERAL_SYMBOLS];
    clear_bytes(lane_counts, sizeof lane_counts);
    for (size_t i = 0; i < count; i += 4) {
        uint32_t four = load_u32(literals + i);
        lane_counts[0][four & 0xFF]++;
        lane_counts[1][(four >> 8) & 0xFF]++;
        lane_counts[2][(four >> 16) & 0xFF]++;
        lane_counts[3][four >> 24]++;
    }
    for (size_t s = 0; s < LITERAL_SYMBOLS; s++) {
        counts[s] = lane_counts[0][s] + lane_counts[1][s] + lane_counts[2][s] + lane_counts[3][s];
    }
}

// Entropy-code the buffered tokens and literals as a `bvx2` block appended to `output`.
static lzfse_zazzcat_status encode_block(block_writer *blocks, lzfse_zazzcat_buffer *output) {
    // The literal stream encodes 4 literals per step, so pad the literals with zeros to a multiple of 4.
    while (blocks->literal_count % 4 != 0) blocks->literals[blocks->literal_count++] = 0;
    const uint8_t *literals = blocks->literals;
    size_t literal_count = blocks->literal_count;
    token *tokens = blocks->tokens;
    size_t token_count = blocks->token_count;

    // A coded distance of 0 means "same as the previous token"; literal-only tokens reuse the previous distance
    // too (or 1 if there is none yet).
    bool have_previous = false;
    uint32_t previous = 0;
    for (size_t t = 0; t < token_count; t++) {
        uint32_t wanted = tokens[t].match_length == 0 ? (have_previous ? previous : 1) : tokens[t].distance;
        if (have_previous && previous == wanted) {
            tokens[t].distance = 0;
        } else {
            tokens[t].distance = wanted;
            previous = wanted;
            have_previous = true;
        }
    }

    // Count symbol occurrences, then scale them into FSE frequencies.
    block_header header;
    // Not cleared: fse_normalize below writes all four frequency tables, and every scalar field is set here.
    // The stream sizes, padding and final states are only known at the end, but write_v2_header reads them
    // first (for the placeholder fixed fields), so they start at 0 and write_v2_fixed_fields rewrites them.
    // clear_bytes(&header, sizeof header);
    header.decoded_size = (uint32_t)blocks->covered_bytes;
    header.literal_count = (uint32_t)literal_count;
    header.token_count = (uint32_t)token_count;
    header.literal_payload_size = 0;
    header.token_payload_size = 0;
    header.literal_padding_bits = 0;
    header.token_padding_bits = 0;
    for (int i = 0; i < 4; i++) header.literal_states[i] = 0;
    header.literal_length_state = 0;
    header.match_length_state = 0;
    header.distance_state = 0;
    uint32_t literal_counts[LITERAL_SYMBOLS];
    count_literals(literals, literal_count, literal_counts);
    uint32_t literal_length_counts[LITERAL_LENGTH_SYMBOLS] = {0};
    uint32_t match_length_counts[MATCH_LENGTH_SYMBOLS] = {0};
    uint32_t distance_counts[DISTANCE_SYMBOLS] = {0};
    for (size_t t = 0; t < token_count; t++) {
        uint32_t l = literal_length_symbol(tokens[t].literal_length);
        uint32_t m = match_length_symbol(tokens[t].match_length);
        uint32_t d = distance_symbol(tokens[t].distance);
        literal_length_counts[l]++;
        match_length_counts[m]++;
        distance_counts[d]++;
        tokens[t].symbols[0] = (uint8_t)l;
        tokens[t].symbols[1] = (uint8_t)m;
        tokens[t].symbols[2] = (uint8_t)d;
    }
    fse_normalize(literal_counts, LITERAL_SYMBOLS, LITERAL_STATES, header.literal_frequencies);
    fse_normalize(literal_length_counts, LITERAL_LENGTH_SYMBOLS, LITERAL_LENGTH_STATES,
                  header.literal_length_frequencies);
    fse_normalize(match_length_counts, MATCH_LENGTH_SYMBOLS, MATCH_LENGTH_STATES, header.match_length_frequencies);
    fse_normalize(distance_counts, DISTANCE_SYMBOLS, DISTANCE_STATES, header.distance_frequencies);
    fse_encoder_entry literal_encoder[LITERAL_SYMBOLS];
    fse_encoder_entry literal_length_encoder[LITERAL_LENGTH_SYMBOLS];
    fse_encoder_entry match_length_encoder[MATCH_LENGTH_SYMBOLS];
    fse_encoder_entry distance_encoder[DISTANCE_SYMBOLS];
    fse_encoder_table(header.literal_frequencies, LITERAL_SYMBOLS, LITERAL_STATES, literal_encoder);
    fse_encoder_table(header.literal_length_frequencies, LITERAL_LENGTH_SYMBOLS, LITERAL_LENGTH_STATES,
                      literal_length_encoder);
    fse_encoder_table(header.match_length_frequencies, MATCH_LENGTH_SYMBOLS, MATCH_LENGTH_STATES, match_length_encoder);
    fse_encoder_table(header.distance_frequencies, DISTANCE_SYMBOLS, DISTANCE_STATES, distance_encoder);

    // The header's size depends only on the frequency tables, so write it now and fill in the rest at the end.
    size_t header_start = output->length;
    size_t header_size = write_v2_header(&header, output);
    if (header_size == 0) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    // Room for both streams: at most 10 bits per literal and 56 bits per token, plus padding and flush slack.
    if (!buffer_reserve(output, literal_count * 10 / 8 + token_count * 8 + 64)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;

    // Literals: four interleaved states, encoded last group first so the decoder reads them forwards.
    size_t literal_stream_start = output->length;
    bit_writer writer;
    bit_writer_start(&writer, output);
    // fse_encode keeps each state offset by the table size; the header gets the states without the offset.
    uint32_t states[4] = {LITERAL_STATES, LITERAL_STATES, LITERAL_STATES, LITERAL_STATES};
    for (size_t group = literal_count; group > 0; group -= 4) {
        const uint8_t *four = literals + group - 4;
        fse_encode(&states[3], &literal_encoder[four[3]], &writer);
        fse_encode(&states[2], &literal_encoder[four[2]], &writer);
        fse_encode(&states[1], &literal_encoder[four[1]], &writer);
        fse_encode(&states[0], &literal_encoder[four[0]], &writer);
        bit_writer_flush(&writer);
    }
    header.literal_padding_bits = bit_writer_finish(&writer, output);
    for (int i = 0; i < 4; i++) header.literal_states[i] = (uint16_t)(states[i] - LITERAL_STATES);
    header.literal_payload_size = (uint32_t)(output->length - literal_stream_start);

    // Tokens: 8 zero bytes first, then each token (last first) as distance, match length and literal length,
    // each with its extra bits written before its state bits.
    size_t token_stream_start = output->length;
    clear_bytes(output->data + output->length, 8);
    output->length += 8;
    bit_writer_start(&writer, output);
    // Offset by the table sizes, as the literal states are.
    uint32_t literal_length_state = LITERAL_LENGTH_STATES, match_length_state = MATCH_LENGTH_STATES;
    uint32_t distance_state = DISTANCE_STATES;
    for (size_t t = token_count; t > 0; t--) {
        const token *tk = &tokens[t - 1];
        uint32_t l = tk->symbols[0], m = tk->symbols[1], d = tk->symbols[2];
        bit_writer_push(&writer, distance_extra_bits[d], tk->distance - distance_base[d]);
        fse_encode(&distance_state, &distance_encoder[d], &writer);
        bit_writer_push(&writer, match_length_extra_bits[m], tk->match_length - match_length_base[m]);
        fse_encode(&match_length_state, &match_length_encoder[m], &writer);
        bit_writer_push(&writer, literal_length_extra_bits[l], tk->literal_length - literal_length_base[l]);
        fse_encode(&literal_length_state, &literal_length_encoder[l], &writer);
        bit_writer_flush(&writer);
    }
    header.token_padding_bits = bit_writer_finish(&writer, output);
    header.literal_length_state = (uint16_t)(literal_length_state - LITERAL_LENGTH_STATES);
    header.match_length_state = (uint16_t)(match_length_state - MATCH_LENGTH_STATES);
    header.distance_state = (uint16_t)(distance_state - DISTANCE_STATES);
    header.token_payload_size = (uint32_t)(output->length - token_stream_start);
    // Only the fixed fields change (final states and stream sizes); the frequency tables written above stay.
    write_v2_fixed_fields(&header, (uint32_t)header_size, output->data + header_start);
    return LZFSE_ZAZZCAT_OK;
}

void block_writer_flush(block_writer *blocks, const uint8_t *source, lzfse_zazzcat_buffer *output) {
    if (blocks->token_count == 0) return;
    size_t mark = output->length;
    if (blocks->status == LZFSE_ZAZZCAT_OK) {
        lzfse_zazzcat_status status = encode_block(blocks, output);
        if (status != LZFSE_ZAZZCAT_OK) {
            output->length = mark;
            blocks->status = status;
        } else if (output->length - mark >= blocks->covered_bytes + UNCOMPRESSED_HEADER_SIZE) {
            // Not smaller than storing the bytes as they are: replace the block with a raw one.
            output->length = mark;
            if (buffer_reserve(output, blocks->covered_bytes + UNCOMPRESSED_HEADER_SIZE)) {
                buffer_push_u32_x2_reserved(output, MAGIC_UNCOMPRESSED, (uint32_t)blocks->covered_bytes);
                buffer_append_reserved(output, source + blocks->block_start, blocks->covered_bytes);
            } else {
                blocks->status = LZFSE_ZAZZCAT_OUT_OF_MEMORY;
            }
        }
    }
    blocks->block_start += blocks->covered_bytes;
    blocks->covered_bytes = 0;
    blocks->token_count = 0;
    blocks->literal_count = 0;
}
