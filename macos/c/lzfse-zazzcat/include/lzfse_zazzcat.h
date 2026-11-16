// lzfse-zazzcat: LZFSE and LZVN compression (Apple's formats), compatible with Apple's libcompression.
//
// Streams this library writes decode with Apple's decoder, and it decodes streams Apple's encoder writes. The
// compressed bytes are not identical to Apple's.
//
// Conventions:
//   - Functions return an lzfse_zazzcat_status: LZFSE_ZAZZCAT_OK (0) or a negative error code.
//   - Output that grows goes into an lzfse_zazzcat_buffer, which the caller owns: start it zeroed
//     (`lzfse_zazzcat_buffer out = {0};`), functions append to it, and lzfse_zazzcat_buffer_free releases it.
//     Reusing one buffer across calls avoids allocating again once it has grown.
//   - Sizes are in bytes.
#ifndef LZFSE_ZAZZCAT_H
#define LZFSE_ZAZZCAT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Result of every call that can fail.
typedef enum lzfse_zazzcat_status {
    LZFSE_ZAZZCAT_OK = 0,
    // The input ended in the middle of a block or before the end-of-stream marker.
    LZFSE_ZAZZCAT_TRUNCATED = -1,
    // The input is not a valid stream (bad magic, header, table or payload).
    LZFSE_ZAZZCAT_CORRUPT = -2,
    // The output does not fit in the buffer or limit the caller gave.
    LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL = -3,
    // A memory allocation failed.
    LZFSE_ZAZZCAT_OUT_OF_MEMORY = -4,
    // An argument was invalid (for example a chunk size of 0).
    LZFSE_ZAZZCAT_INVALID_ARGUMENT = -5,
    // The operation needs an operating system feature this system lacks (decmpfs files outside macOS).
    LZFSE_ZAZZCAT_UNSUPPORTED = -6,
    // A system call failed; errno says why.
    LZFSE_ZAZZCAT_SYSTEM_ERROR = -7,
    // The kernel accepted every call but the file does not read as compressed with the expected size.
    LZFSE_ZAZZCAT_NOT_APPLIED = -8,
} lzfse_zazzcat_status;

// A few words describing a status, for error messages.
const char *lzfse_zazzcat_status_string(lzfse_zazzcat_status status);

// A growable byte array: `data[0 .. length)` holds the bytes, `capacity` is the allocated size.
typedef struct lzfse_zazzcat_buffer {
    uint8_t *data;
    size_t length;
    size_t capacity;
} lzfse_zazzcat_buffer;

// Release a buffer's memory and reset it to empty.
void lzfse_zazzcat_buffer_free(lzfse_zazzcat_buffer *buffer);

// --- Encoding ---------------------------------------------------------------------------------------------------

// Inputs shorter than this are encoded as one LZVN block, as Apple's encoder does.
#define LZFSE_ZAZZCAT_LZVN_THRESHOLD 4096

// Encoder tuning knobs. lzfse_zazzcat_default_options() returns the tuned defaults.
typedef struct lzfse_zazzcat_options {
    // log2 of the number of match-finder hash buckets for inputs over 64 KiB (each bucket holds 4 candidates);
    // the second table, keyed on 8 bytes, has four times as many single-position slots. Default 14.
    uint32_t hash_bits;
    // Stop searching once a match is at least this long. Default 40.
    size_t good_match;
    // Before taking a match shorter than this, try one byte later (0 disables). Default 8.
    size_t lazy_below;
    // After 2^skip_shift positions in a row without a match, step further ahead (0 disables). Default 6.
    uint32_t skip_shift;
} lzfse_zazzcat_options;

// The default options.
lzfse_zazzcat_options lzfse_zazzcat_default_options(void);

// A reusable encoder. Its match-finder tables and block buffers carry over between calls, so encoding many
// inputs (such as 64 KiB chunks) allocates nothing once warmed up. One encoder must not be used by two threads
// at the same time.
typedef struct lzfse_zazzcat_encoder lzfse_zazzcat_encoder;

// Create an encoder (NULL options means the defaults). Returns NULL if memory runs out.
lzfse_zazzcat_encoder *lzfse_zazzcat_encoder_create(const lzfse_zazzcat_options *options);

// Destroy an encoder (NULL is allowed).
void lzfse_zazzcat_encoder_destroy(lzfse_zazzcat_encoder *encoder);

// Change the options used by later calls.
void lzfse_zazzcat_encoder_set_options(lzfse_zazzcat_encoder *encoder, const lzfse_zazzcat_options *options);

// Append the complete LZFSE stream for `source[0 .. source_length)` to `output`.
lzfse_zazzcat_status lzfse_zazzcat_encoder_encode(lzfse_zazzcat_encoder *encoder, const void *source,
                                                  size_t source_length, lzfse_zazzcat_buffer *output);

// One-shot form of lzfse_zazzcat_encoder_encode with a temporary encoder (NULL options means the defaults).
lzfse_zazzcat_status lzfse_zazzcat_encode(const void *source, size_t source_length,
                                          const lzfse_zazzcat_options *options, lzfse_zazzcat_buffer *output);

// --- Decoding ---------------------------------------------------------------------------------------------------

// A reusable decoder: its tables and literal buffer carry over between calls. One decoder must not be used by two
// threads at the same time.
typedef struct lzfse_zazzcat_decoder lzfse_zazzcat_decoder;

// Create a decoder. Returns NULL if memory runs out.
lzfse_zazzcat_decoder *lzfse_zazzcat_decoder_create(void);

// Destroy a decoder (NULL is allowed).
void lzfse_zazzcat_decoder_destroy(lzfse_zazzcat_decoder *decoder);

// Decode a stream into `destination[0 .. destination_capacity)` and store the number of bytes written in
// `*decoded_length`. Like Apple's decoder, this may overwrite bytes past the decoded length (it uses wide copies
// when there is room); their contents are unspecified.
lzfse_zazzcat_status lzfse_zazzcat_decoder_decode(lzfse_zazzcat_decoder *decoder, const void *source,
                                                  size_t source_length, void *destination, size_t destination_capacity,
                                                  size_t *decoded_length);

// One-shot form of lzfse_zazzcat_decoder_decode with a temporary decoder.
lzfse_zazzcat_status lzfse_zazzcat_decode_into(const void *source, size_t source_length, void *destination,
                                               size_t destination_capacity, size_t *decoded_length);

// Read only the block headers of a stream and store its decoded size in `*length`.
lzfse_zazzcat_status lzfse_zazzcat_decoded_length(const void *source, size_t source_length, size_t *length);

// Decode a whole stream and append it to `output`, refusing to produce more than `limit` bytes.
lzfse_zazzcat_status lzfse_zazzcat_decode(const void *source, size_t source_length, size_t limit,
                                          lzfse_zazzcat_buffer *output);

// --- Parallel chunked encoding ----------------------------------------------------------------------------------

// A growable array of sizes, used for the offsets of chunk streams.
typedef struct lzfse_zazzcat_offsets {
    size_t *data;
    size_t length;
    size_t capacity;
} lzfse_zazzcat_offsets;

// Release an offsets array's memory and reset it to empty.
void lzfse_zazzcat_offsets_free(lzfse_zazzcat_offsets *offsets);

// Number of threads the chunked encoder uses (1 in builds without threads).
size_t lzfse_zazzcat_parallelism(void);

// Compress `source` in independent `chunk_size`-byte pieces on all cores, appending the streams back to back to
// `output`. `offsets` is cleared and filled with n + 1 positions in `output`: stream i occupies
// output->data[offsets->data[i] .. offsets->data[i + 1]). This is the layout of an APFS resource fork. Once
// `output`, `offsets` and the worker threads have warmed up, repeated calls allocate nothing.
lzfse_zazzcat_status lzfse_zazzcat_encode_chunks(const void *source, size_t source_length, size_t chunk_size,
                                                 const lzfse_zazzcat_options *options, lzfse_zazzcat_buffer *output,
                                                 lzfse_zazzcat_offsets *offsets);

// --- LZVN -------------------------------------------------------------------------------------------------------

// Append `source` compressed as a raw LZVN payload (ending with the end-of-stream opcode) to `output`.
lzfse_zazzcat_status lzfse_zazzcat_lzvn_encode(const void *source, size_t source_length, lzfse_zazzcat_buffer *output);

// Decode a raw LZVN payload into `destination[0 .. destination_capacity)` and store the number of bytes written in
// `*decoded_length`. Like lzfse_zazzcat_decoder_decode, this may overwrite bytes past the decoded length (it uses
// wide copies when there is room); their contents are unspecified.
lzfse_zazzcat_status lzfse_zazzcat_lzvn_decode(const void *source, size_t source_length, void *destination,
                                               size_t destination_capacity, size_t *decoded_length);

// --- APFS transparent compression (decmpfs) ---------------------------------------------------------------------
//
// A compressed file has an empty data fork, a `com.apple.decmpfs` extended attribute starting with a 16-byte
// header, and, for the resource-fork types, its data in `com.apple.ResourceFork`: a table of little-endian 32-bit
// offsets (one per 64 KiB chunk plus one for the end) followed by the chunks. lzfse_zazzcat_decmpfs_compress builds
// those bytes and lzfse_zazzcat_decmpfs_apply puts them on a file (macOS only).

// Uncompressed size of each chunk.
#define LZFSE_ZAZZCAT_DECMPFS_CHUNK 65536
// Largest `com.apple.decmpfs` attribute the kernel accepts.
#define LZFSE_ZAZZCAT_DECMPFS_MAX_XATTR 3802
// Compression types: data stored inline in the attribute, or in the resource fork.
#define LZFSE_ZAZZCAT_DECMPFS_LZVN_INLINE  7
#define LZFSE_ZAZZCAT_DECMPFS_LZVN_FORK    8
#define LZFSE_ZAZZCAT_DECMPFS_LZFSE_INLINE 11
#define LZFSE_ZAZZCAT_DECMPFS_LZFSE_FORK   12

// The two attributes that make a file transparently compressed.
typedef struct lzfse_zazzcat_decmpfs {
    // Contents of `com.apple.decmpfs`.
    lzfse_zazzcat_buffer xattr;
    // Contents of `com.apple.ResourceFork`; empty for the inline types.
    lzfse_zazzcat_buffer fork;
} lzfse_zazzcat_decmpfs;

// Compress file contents into decmpfs attributes (LZFSE, using all cores). `result` is overwritten; free it with
// lzfse_zazzcat_decmpfs_free.
lzfse_zazzcat_status lzfse_zazzcat_decmpfs_compress(const void *data, size_t data_length,
                                                    lzfse_zazzcat_decmpfs *result);

// Release both attribute buffers.
void lzfse_zazzcat_decmpfs_free(lzfse_zazzcat_decmpfs *compressed);

// Make the open file `fd` (opened for writing) a transparently compressed file holding `compressed`, which must
// have been made from that file's current contents: set `com.apple.ResourceFork` (or remove a stale one for the
// inline types), set `com.apple.decmpfs`, empty the data fork and add the compressed flag. Then check that the
// kernel reports the flag and the original size. This empties the file's data before the kernel has accepted the
// compressed form, so apply it to a copy and swap the copy in only after reading it back.
// On LZFSE_ZAZZCAT_SYSTEM_ERROR errno holds the reason; on any failure `*failed_step` (if not NULL) names the step
// ("setting com.apple.ResourceFork", for example), as a static string.
// Returns LZFSE_ZAZZCAT_UNSUPPORTED outside macOS.
lzfse_zazzcat_status lzfse_zazzcat_decmpfs_apply(int fd, const lzfse_zazzcat_decmpfs *compressed,
                                                 const char **failed_step);

// Read the compression type and uncompressed size from a `com.apple.decmpfs` attribute.
lzfse_zazzcat_status lzfse_zazzcat_decmpfs_parse_header(const void *xattr, size_t xattr_length, uint32_t *type,
                                                        uint64_t *uncompressed_size);

// Decompress a file from its decmpfs attributes (LZFSE or LZVN types), appending it to `output`, refusing sizes
// above `limit`.
lzfse_zazzcat_status lzfse_zazzcat_decmpfs_decompress(const void *xattr, size_t xattr_length, const void *fork,
                                                      size_t fork_length, uint64_t limit, lzfse_zazzcat_buffer *output);

// --- Stream statistics (diagnostics) ----------------------------------------------------------------------------

// Number of match-length and distance buckets.
#define LZFSE_ZAZZCAT_LENGTH_BUCKETS   8
#define LZFSE_ZAZZCAT_DISTANCE_BUCKETS 6

// Upper bounds (exclusive) of the buckets.
extern const uint32_t lzfse_zazzcat_length_bucket_limits[LZFSE_ZAZZCAT_LENGTH_BUCKETS];
extern const uint32_t lzfse_zazzcat_distance_bucket_limits[LZFSE_ZAZZCAT_DISTANCE_BUCKETS];

// Totals and distributions of one or more streams, read from their block headers and match streams without
// decoding any output. Start it zeroed and add streams with lzfse_zazzcat_stream_stats_add.
typedef struct lzfse_zazzcat_stream_stats {
    uint64_t decoded_bytes;
    uint64_t stored_bytes;
    // Blocks by type: [0] LZFSE (bvx1/bvx2), [1] LZVN (bvxn), [2] uncompressed (bvx-).
    uint64_t blocks[3];
    // Bytes of LZFSE block headers, literal streams, match streams, and of the other block types.
    uint64_t header_bytes;
    uint64_t literal_bytes;
    uint64_t match_stream_bytes;
    uint64_t other_block_bytes;
    // Literals and tokens in LZFSE blocks.
    uint64_t literals;
    uint64_t tokens;
    // Tokens with no match, and matches that reuse the previous distance.
    uint64_t literal_only_tokens;
    uint64_t repeat_distances;
    // Matches per bucket, and the bytes they cover.
    uint64_t matches_by_length[LZFSE_ZAZZCAT_LENGTH_BUCKETS];
    uint64_t bytes_by_length[LZFSE_ZAZZCAT_LENGTH_BUCKETS];
    uint64_t matches_by_distance[LZFSE_ZAZZCAT_DISTANCE_BUCKETS];
    uint64_t bytes_by_distance[LZFSE_ZAZZCAT_DISTANCE_BUCKETS];
} lzfse_zazzcat_stream_stats;

// Add the blocks of one complete stream to `stats`.
lzfse_zazzcat_status lzfse_zazzcat_stream_stats_add(lzfse_zazzcat_stream_stats *stats, const void *stream,
                                                    size_t stream_length);

#ifdef __cplusplus
}
#endif

#endif
