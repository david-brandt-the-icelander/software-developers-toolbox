// APFS/HFS+ transparent compression containers (decmpfs) for LZFSE (types 11 and 12) and LZVN (types 7 and 8).
// macOS: fchflags, st_flags, UF_COMPRESSED and the extended-attribute calls, used by lzfse_zazzcat_decmpfs_apply.
#define _DARWIN_C_SOURCE
#include "buffer.h"
#include "format.h"
#include "lzvn.h"

#define DECMPFS_HEADER_SIZE 16
// On-disk header magic, "fpmc".
static const uint8_t decmpfs_magic[4] = {'f', 'p', 'm', 'c'};

// Append the 16-byte `com.apple.decmpfs` header: magic, type and uncompressed size (into reserved capacity).
static void push_header(lzfse_zazzcat_buffer *xattr, uint32_t type, uint64_t size) {
    buffer_append_reserved(xattr, decmpfs_magic, 4);
    buffer_push_u32_reserved(xattr, type);
    buffer_push_u64_reserved(xattr, size);
}

void lzfse_zazzcat_decmpfs_free(lzfse_zazzcat_decmpfs *compressed) {
    lzfse_zazzcat_buffer_free(&compressed->xattr);
    lzfse_zazzcat_buffer_free(&compressed->fork);
}

lzfse_zazzcat_status lzfse_zazzcat_decmpfs_compress(const void *data, size_t length, lzfse_zazzcat_decmpfs *result) {
    clear_bytes(result, sizeof *result);
    size_t chunk_count = div_ceil(length, LZFSE_ZAZZCAT_DECMPFS_CHUNK);
    size_t table_size = 4 * (chunk_count + 1);
    // Encode the chunks straight into the fork after room for the offset table, then fill in the table.
    lzfse_zazzcat_buffer *fork = &result->fork;
    if (!buffer_reserve(fork, table_size)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    // Not cleared: lzfse_zazzcat_encode_chunks always returns chunk_count + 1 offsets, and the resource-fork
    // path below stores every one of them, filling all table_size bytes. The inline path sets fork->length to 0,
    // so the table is dropped. On failure the fork is freed.
    // clear_bytes(fork->data, table_size);
    fork->length = table_size;
    lzfse_zazzcat_offsets offsets = {NULL, 0, 0};
    lzfse_zazzcat_status status =
        lzfse_zazzcat_encode_chunks(data, length, LZFSE_ZAZZCAT_DECMPFS_CHUNK, NULL, fork, &offsets);
    if (status == LZFSE_ZAZZCAT_OK && !buffer_reserve(&result->xattr, DECMPFS_HEADER_SIZE + fork->length)) {
        status = LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    }
    if (status != LZFSE_ZAZZCAT_OK) {
        lzfse_zazzcat_offsets_free(&offsets);
        lzfse_zazzcat_decmpfs_free(result);
        return status;
    }
    size_t stream_bytes = fork->length - table_size;
    if (chunk_count == 1 && DECMPFS_HEADER_SIZE + stream_bytes <= LZFSE_ZAZZCAT_DECMPFS_MAX_XATTR) {
        // One small chunk fits inline in the attribute; the resource fork stays empty.
        push_header(&result->xattr, LZFSE_ZAZZCAT_DECMPFS_LZFSE_INLINE, length);
        buffer_append_reserved(&result->xattr, fork->data + table_size, stream_bytes);
        fork->length = 0;
    } else {
        push_header(&result->xattr, LZFSE_ZAZZCAT_DECMPFS_LZFSE_FORK, length);
        for (size_t i = 0; i < offsets.length; i++) store_u32(fork->data + 4 * i, (uint32_t)offsets.data[i]);
    }
    lzfse_zazzcat_offsets_free(&offsets);
    return LZFSE_ZAZZCAT_OK;
}

lzfse_zazzcat_status lzfse_zazzcat_decmpfs_parse_header(const void *xattr_bytes, size_t xattr_length, uint32_t *type,
                                                        uint64_t *uncompressed_size) {
    const uint8_t *xattr = xattr_bytes;
    // Compare the 4 magic bytes as one 32-bit integer: a single unaligned load and one compare against a
    // constant (load_u32 of the static const magic folds to an immediate).
    if (xattr_length < DECMPFS_HEADER_SIZE || load_u32(xattr) != load_u32(decmpfs_magic)) return LZFSE_ZAZZCAT_CORRUPT;
    *type = load_u32(xattr + 4);
    *uncompressed_size = load_u64(xattr + 8);
    return LZFSE_ZAZZCAT_OK;
}

// Decode one chunk of compression `type` into `destination[0 .. size)`. A chunk that is not a stream may be stored
// raw after a one-byte marker (0x06 for LZVN, any non-'b' byte for LZFSE).
static lzfse_zazzcat_status decode_chunk(uint32_t type, const uint8_t *chunk, size_t chunk_length, uint8_t *destination,
                                         size_t size) {
    bool lzfse = type == LZFSE_ZAZZCAT_DECMPFS_LZFSE_INLINE || type == LZFSE_ZAZZCAT_DECMPFS_LZFSE_FORK;
    bool raw = false;
    if (chunk_length == 0) {
        if (lzfse) return LZFSE_ZAZZCAT_CORRUPT;
    } else if (lzfse) {
        if (chunk[0] != 'b') {
            if (chunk_length != size + 1) return LZFSE_ZAZZCAT_CORRUPT;
            raw = true;
        }
    } else if (chunk[0] == 0x06 && chunk_length == size + 1) {
        raw = true;
    }
    if (raw) {
        copy_bytes(destination, chunk + 1, size);
        return LZFSE_ZAZZCAT_OK;
    }
    size_t decoded = 0;
    lzfse_zazzcat_status status = lzfse ? lzfse_zazzcat_decode_into(chunk, chunk_length, destination, size, &decoded)
                                        : lzfse_zazzcat_lzvn_decode(chunk, chunk_length, destination, size, &decoded);
    if (status != LZFSE_ZAZZCAT_OK) return status;
    return decoded == size ? LZFSE_ZAZZCAT_OK : LZFSE_ZAZZCAT_CORRUPT;
}

lzfse_zazzcat_status lzfse_zazzcat_decmpfs_decompress(const void *xattr_bytes, size_t xattr_length,
                                                      const void *fork_bytes, size_t fork_length, uint64_t limit,
                                                      lzfse_zazzcat_buffer *output) {
    const uint8_t *xattr = xattr_bytes, *fork = fork_bytes;
    uint32_t type;
    uint64_t size64;
    lzfse_zazzcat_status status = lzfse_zazzcat_decmpfs_parse_header(xattr, xattr_length, &type, &size64);
    if (status != LZFSE_ZAZZCAT_OK) return status;
    if (size64 > limit || size64 > SIZE_MAX) return LZFSE_ZAZZCAT_OUTPUT_TOO_SMALL;
    size_t size = (size_t)size64;
    if (!buffer_reserve(output, size)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    uint8_t *out = output->data + output->length;
    if (type == LZFSE_ZAZZCAT_DECMPFS_LZFSE_INLINE || type == LZFSE_ZAZZCAT_DECMPFS_LZVN_INLINE) {
        status = decode_chunk(type, xattr + DECMPFS_HEADER_SIZE, xattr_length - DECMPFS_HEADER_SIZE, out, size);
    } else if (type == LZFSE_ZAZZCAT_DECMPFS_LZFSE_FORK || type == LZFSE_ZAZZCAT_DECMPFS_LZVN_FORK) {
        size_t chunk_count = div_ceil(size, LZFSE_ZAZZCAT_DECMPFS_CHUNK);
        for (size_t i = 0; i < chunk_count && status == LZFSE_ZAZZCAT_OK; i++) {
            // The chunk's start and end come from the offset table at the start of the fork.
            uint32_t start, end;
            if ((status = read_u32_at(fork, fork_length, 4 * i, &start)) ||
                (status = read_u32_at(fork, fork_length, 4 * i + 4, &end))) {
                break;
            }
            if (start > end || end > fork_length) {
                status = LZFSE_ZAZZCAT_CORRUPT;
                break;
            }
            size_t at = i * LZFSE_ZAZZCAT_DECMPFS_CHUNK;
            status = decode_chunk(type, fork + start, end - start, out + at,
                                  min_size(LZFSE_ZAZZCAT_DECMPFS_CHUNK, size - at));
        }
    } else {
        status = LZFSE_ZAZZCAT_CORRUPT;
    }
    if (status != LZFSE_ZAZZCAT_OK) return status;
    output->length += size;
    return LZFSE_ZAZZCAT_OK;
}

#ifdef __APPLE__
#include <errno.h>
#include <sys/stat.h>
#include <sys/xattr.h>
#include <unistd.h>

// Record which step of lzfse_zazzcat_decmpfs_apply failed, for callers that asked.
static lzfse_zazzcat_status apply_failed(const char **failed_step, const char *step, lzfse_zazzcat_status status) {
    if (failed_step != NULL) *failed_step = step;
    return status;
}

lzfse_zazzcat_status lzfse_zazzcat_decmpfs_apply(int fd, const lzfse_zazzcat_decmpfs *compressed,
                                                 const char **failed_step) {
    uint32_t type;
    uint64_t size;
    lzfse_zazzcat_status status =
        lzfse_zazzcat_decmpfs_parse_header(compressed->xattr.data, compressed->xattr.length, &type, &size);
    if (status != LZFSE_ZAZZCAT_OK) return apply_failed(failed_step, "reading the decmpfs header", status);
    // Calls on an open descriptor never follow links, so no XATTR_NOFOLLOW: the f* calls reject it (EINVAL).
    if (compressed->fork.length > 0) {
        if (fsetxattr(fd, "com.apple.ResourceFork", compressed->fork.data, compressed->fork.length, 0, 0) != 0) {
            return apply_failed(failed_step, "setting com.apple.ResourceFork", LZFSE_ZAZZCAT_SYSTEM_ERROR);
        }
    } else if (fremovexattr(fd, "com.apple.ResourceFork", 0) != 0 && errno != ENOATTR) {
        // An inline file must not keep an old resource fork the kernel might read instead.
        return apply_failed(failed_step, "removing an old com.apple.ResourceFork", LZFSE_ZAZZCAT_SYSTEM_ERROR);
    }
    if (fsetxattr(fd, "com.apple.decmpfs", compressed->xattr.data, compressed->xattr.length, 0, 0) != 0) {
        return apply_failed(failed_step, "setting com.apple.decmpfs", LZFSE_ZAZZCAT_SYSTEM_ERROR);
    }
    // A compressed file's data fork is empty: the kernel serves its contents from the attributes.
    if (ftruncate(fd, 0) != 0) return apply_failed(failed_step, "emptying the data fork", LZFSE_ZAZZCAT_SYSTEM_ERROR);
    struct stat info;
    if (fstat(fd, &info) != 0) return apply_failed(failed_step, "reading the file flags", LZFSE_ZAZZCAT_SYSTEM_ERROR);
    if (fchflags(fd, info.st_flags | UF_COMPRESSED) != 0) {
        return apply_failed(failed_step, "setting the compressed flag", LZFSE_ZAZZCAT_SYSTEM_ERROR);
    }
    // The kernel can accept the flag and still not treat the file as compressed (an unknown type, for example).
    if (fstat(fd, &info) != 0) return apply_failed(failed_step, "checking the result", LZFSE_ZAZZCAT_SYSTEM_ERROR);
    if (!(info.st_flags & UF_COMPRESSED) || (uint64_t)info.st_size != size) {
        return apply_failed(failed_step, "checking the result", LZFSE_ZAZZCAT_NOT_APPLIED);
    }
    return LZFSE_ZAZZCAT_OK;
}

#else

lzfse_zazzcat_status lzfse_zazzcat_decmpfs_apply(int fd, const lzfse_zazzcat_decmpfs *compressed,
                                                 const char **failed_step) {
    (void)fd;
    (void)compressed;
    if (failed_step != NULL) *failed_step = "decmpfs files exist only on macOS";
    return LZFSE_ZAZZCAT_UNSUPPORTED;
}

#endif
