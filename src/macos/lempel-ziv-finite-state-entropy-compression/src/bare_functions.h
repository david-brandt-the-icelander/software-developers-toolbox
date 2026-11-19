// "Bare" versions of functions whose return value (or out value) a caller deliberately ignores. Each bare_ function
// has the signature the caller actually wants (no result), calls the real function and discards the result here,
// in one place. Callers never write a bare call that drops a result, and never write `(void)call()`; they call the
// bare_ version instead. A bare_ function is named `bare_` + the real function's name.
#ifndef LZFSE_ZAZZCAT_BARE_FUNCTIONS_H
#define LZFSE_ZAZZCAT_BARE_FUNCTIONS_H

#include <stdarg.h>
#include <stdio.h>
#include <time.h>

#include "bits.h"
#include "buffer.h"
#include "format.h"
#include "fse.h"
#include "lzfse_zazzcat.h"

// The pthread wrappers are left out of a library built without threads, unless the including program starts
// threads itself (the benchmark does) and defines LZFSE_ZAZZCAT_PROGRAM_USES_THREADS first.
#if !defined(LZFSE_ZAZZCAT_NO_THREADS) || defined(LZFSE_ZAZZCAT_PROGRAM_USES_THREADS)
#define LZFSE_ZAZZCAT_HAS_PTHREAD_WRAPPERS
#include <pthread.h>
#endif
#include <dirent.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <unistd.h>

// --- Public API ----------------------------------------------------------------------------------------------

static inline void bare_lzfse_zazzcat_encoder_encode(lzfse_zazzcat_encoder *encoder, const void *source,
                                                     size_t source_length, lzfse_zazzcat_buffer *output) {
    (void)lzfse_zazzcat_encoder_encode(encoder, source, source_length, output);
}

static inline void bare_lzfse_zazzcat_encode(const void *source, size_t source_length,
                                             const lzfse_zazzcat_options *options, lzfse_zazzcat_buffer *output) {
    (void)lzfse_zazzcat_encode(source, source_length, options, output);
}

static inline void bare_lzfse_zazzcat_decoder_decode(lzfse_zazzcat_decoder *decoder, const void *source,
                                                     size_t source_length, void *destination,
                                                     size_t destination_capacity, size_t *decoded_length) {
    (void)lzfse_zazzcat_decoder_decode(decoder, source, source_length, destination, destination_capacity,
                                       decoded_length);
}

static inline void bare_lzfse_zazzcat_decode_into(const void *source, size_t source_length, void *destination,
                                                  size_t destination_capacity, size_t *decoded_length) {
    (void)lzfse_zazzcat_decode_into(source, source_length, destination, destination_capacity, decoded_length);
}

static inline void bare_lzfse_zazzcat_decode(const void *source, size_t source_length, size_t limit,
                                             lzfse_zazzcat_buffer *output) {
    (void)lzfse_zazzcat_decode(source, source_length, limit, output);
}

static inline void bare_lzfse_zazzcat_encode_chunks(const void *source, size_t source_length, size_t chunk_size,
                                                    const lzfse_zazzcat_options *options, lzfse_zazzcat_buffer *output,
                                                    lzfse_zazzcat_offsets *offsets) {
    (void)lzfse_zazzcat_encode_chunks(source, source_length, chunk_size, options, output, offsets);
}

static inline void bare_lzfse_zazzcat_lzvn_encode(const void *source, size_t source_length,
                                                  lzfse_zazzcat_buffer *output) {
    (void)lzfse_zazzcat_lzvn_encode(source, source_length, output);
}

static inline void bare_lzfse_zazzcat_lzvn_decode(const void *source, size_t source_length, void *destination,
                                                  size_t destination_capacity, size_t *decoded_length) {
    (void)lzfse_zazzcat_lzvn_decode(source, source_length, destination, destination_capacity, decoded_length);
}

static inline void bare_lzfse_zazzcat_decmpfs_parse_header(const void *xattr, size_t xattr_length, uint32_t *type,
                                                           uint64_t *uncompressed_size) {
    (void)lzfse_zazzcat_decmpfs_parse_header(xattr, xattr_length, type, uncompressed_size);
}

static inline void bare_lzfse_zazzcat_decmpfs_decompress(const void *xattr, size_t xattr_length, const void *fork,
                                                         size_t fork_length, uint64_t limit,
                                                         lzfse_zazzcat_buffer *output) {
    (void)lzfse_zazzcat_decmpfs_decompress(xattr, xattr_length, fork, fork_length, limit, output);
}

static inline void bare_lzfse_zazzcat_stream_stats_add(lzfse_zazzcat_stream_stats *stats, const void *stream,
                                                       size_t stream_length) {
    (void)lzfse_zazzcat_stream_stats_add(stats, stream, stream_length);
}

// --- Internal functions --------------------------------------------------------------------------------------

static inline void bare_buffer_reserve(lzfse_zazzcat_buffer *buffer, size_t extra) {
    (void)buffer_reserve(buffer, extra);
}

static inline void bare_buffer_append(lzfse_zazzcat_buffer *buffer, const void *bytes, size_t count) {
    (void)buffer_append(buffer, bytes, count);
}

static inline void bare_write_v1_header(const block_header *header, lzfse_zazzcat_buffer *output) {
    (void)write_v1_header(header, output);
}

static inline void bare_write_v2_header(const block_header *header, lzfse_zazzcat_buffer *output) {
    (void)write_v2_header(header, output);
}

static inline void bare_bit_reader_pull(bit_reader *reader, uint32_t count) {
    (void)bit_reader_pull(reader, count);
}

static inline void bare_fse_decode_value(uint32_t *state, const fse_value_entry *table, uint32_t state_mask,
                                         bit_reader *reader) {
    (void)fse_decode_value(state, table, state_mask, reader);
}

// --- C library and POSIX -------------------------------------------------------------------------------------

static inline void bare_fclose(FILE *file) {
    (void)fclose(file);
}

static inline void bare_pclose(FILE *pipe) {
    (void)pclose(pipe);
}

static inline void bare_close(int descriptor) {
    (void)close(descriptor);
}

static inline void bare_closedir(DIR *directory) {
    (void)closedir(directory);
}

static inline void bare_mkdir(const char *path, mode_t mode) {
    (void)mkdir(path, mode);
}

static inline void bare_rmdir(const char *path) {
    (void)rmdir(path);
}

static inline void bare_unlink(const char *path) {
    (void)unlink(path);
}

static inline void bare_lstat(const char *path, struct stat *info) {
    (void)lstat(path, info);
}

static inline void bare_uname(struct utsname *system) {
    (void)uname(system);
}

static inline void bare_clock_gettime(clockid_t clock, struct timespec *time) {
    (void)clock_gettime(clock, time);
}

static inline void bare_fputs(const char *text, FILE *stream) {
    (void)fputs(text, stream);
}

// printf-style output whose character count is not wanted.
static inline void bare_printf(const char *format, ...) __attribute__((format(printf, 1, 2)));
static inline void bare_printf(const char *format, ...) {
    va_list arguments;
    va_start(arguments, format);
    (void)vprintf(format, arguments);
    va_end(arguments);
}

static inline void bare_fprintf(FILE *stream, const char *format, ...) __attribute__((format(printf, 2, 3)));
static inline void bare_fprintf(FILE *stream, const char *format, ...) {
    va_list arguments;
    va_start(arguments, format);
    (void)vfprintf(stream, format, arguments);
    va_end(arguments);
}

#ifdef LZFSE_ZAZZCAT_HAS_PTHREAD_WRAPPERS
static inline void bare_pthread_create(pthread_t *thread, const pthread_attr_t *attributes, void *(*start)(void *),
                                       void *argument) {
    (void)pthread_create(thread, attributes, start, argument);
}

static inline void bare_pthread_join(pthread_t thread, void **result) {
    (void)pthread_join(thread, result);
}

static inline void bare_pthread_detach(pthread_t thread) {
    (void)pthread_detach(thread);
}

static inline void bare_pthread_once(pthread_once_t *once, void (*initialize)(void)) {
    (void)pthread_once(once, initialize);
}

static inline void bare_pthread_mutex_init(pthread_mutex_t *mutex, const pthread_mutexattr_t *attributes) {
    (void)pthread_mutex_init(mutex, attributes);
}

static inline void bare_pthread_mutex_lock(pthread_mutex_t *mutex) {
    (void)pthread_mutex_lock(mutex);
}

static inline void bare_pthread_mutex_unlock(pthread_mutex_t *mutex) {
    (void)pthread_mutex_unlock(mutex);
}

static inline void bare_pthread_cond_init(pthread_cond_t *condition, const pthread_condattr_t *attributes) {
    (void)pthread_cond_init(condition, attributes);
}

static inline void bare_pthread_cond_wait(pthread_cond_t *condition, pthread_mutex_t *mutex) {
    (void)pthread_cond_wait(condition, mutex);
}

static inline void bare_pthread_cond_signal(pthread_cond_t *condition) {
    (void)pthread_cond_signal(condition);
}

static inline void bare_pthread_cond_broadcast(pthread_cond_t *condition) {
    (void)pthread_cond_broadcast(condition);
}
#endif

#endif
