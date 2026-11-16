// Hot copy and compare loops shared by the encoders and decoders.
//
// "Wide" copies move 8 or 16 bytes per step and may write up to 15 bytes past the requested end. Callers leave
// that much spare room (COPY_SLACK) after the destination, which later output overwrites; the decoders check the
// room once per token instead of once per byte.
#ifndef LZFSE_ZAZZCAT_COPY_H
#define LZFSE_ZAZZCAT_COPY_H

#include "common.h"

// Spare bytes the wide copies may write past the requested end; callers keep this much room.
#define COPY_SLACK 32

// Number of equal bytes at buffer[earlier ...] and buffer[later ...] (earlier < later), stopping at `end`
// (an index into the same buffer). Compares 8 bytes at a time.
ALWAYS_INLINE size_t match_length(const uint8_t *buffer, size_t earlier, size_t later, size_t end) {
    size_t limit = end - later;
    size_t n = 0;
    while (n + 8 <= limit) {
        uint64_t difference = load_u64(buffer + earlier + n) ^ load_u64(buffer + later + n);
        // The lowest set bit of the XOR marks the first differing byte (the data is little-endian);
        // __builtin_ctzll counts the zero bits below it.
        if (difference != 0) return n + (size_t)(__builtin_ctzll(difference) / 8);
        n += 8;
    }
    while (n < limit && buffer[earlier + n] == buffer[later + n]) n++;
    return n;
}

// Number of equal bytes going backwards from just before `earlier` and `later`, at most `limit`.
ALWAYS_INLINE size_t match_length_backward(const uint8_t *buffer, size_t earlier, size_t later, size_t limit) {
    size_t n = 0;
    while (n < limit && n < earlier && buffer[earlier - 1 - n] == buffer[later - 1 - n]) n++;
    return n;
}

// Copy `length` bytes from `distance` bytes back to `output + position`, repeating the pattern when the ranges
// overlap (distance < length), exactly (nothing written past the end). The caller guarantees
// 1 <= distance <= position. The pattern is copied in growing pieces, each a whole number of periods.
//
// No piece's source and destination overlap, so plain copy_bytes is safe and memmove's overlap handling is not
// needed (the sanitizer build checks this). A piece of `n` bytes copies from `position - distance` to
// `position + done`, so the two overlap only when n > distance + done. The direct call has n = length <= distance
// and done = 0. In the loop the first piece is `distance` with done = 0; after it done >= distance, and each later
// piece is at most `done - done % distance`, which is <= done.
ALWAYS_INLINE void copy_match_exact(uint8_t *output, size_t position, size_t distance, size_t length) {
    if (distance >= length) {
        copy_bytes(output + position, output + position - distance, length);
        return;
    }
    size_t done = 0;
    size_t piece = distance;
    while (done < length) {
        size_t n = min_size(piece, length - done);
        copy_bytes(output + position + done, output + position - distance, n);
        done += n;
        piece = max_size(done, distance) - done % distance;
        if (piece == 0) piece = distance;
    }
}

// Copy 16 bytes with one unaligned load and one store (memcpy of a constant size compiles to that).
ALWAYS_INLINE void copy16(uint8_t *destination, const uint8_t *source) {
    copy_bytes(destination, source, 16);
}

// Copy 8 bytes with one unaligned load and one store.
ALWAYS_INLINE void copy8(uint8_t *destination, const uint8_t *source) {
    copy_bytes(destination, source, 8);
}

// Copy `length` bytes in 16-byte steps, possibly writing up to 15 bytes past the end. Both buffers must have
// `length + 16` bytes available from the given pointers. The first two steps are unrolled because short copies
// are the common case, and a plain loop would be turned into a call to memcpy.
ALWAYS_INLINE void wild_copy(uint8_t *destination, const uint8_t *source, size_t length) {
    copy16(destination, source);
    if (length > 16) {
        copy16(destination + 16, source + 16);
        for (size_t done = 32; done < length; done += 16) copy16(destination + done, source + done);
    }
}

// Copy exactly `length` bytes, reading and writing nothing outside them, with inline loads and stores instead of a
// call to memcpy: 16-byte steps, then one last 16-byte step that ends exactly at the end (it may overlap the step
// before it, which is harmless because the source and destination do not overlap). Shorter copies use two
// overlapping 8- or 4-byte steps, or three single bytes, which together cover any length from 1 to 3.
//
// For the encoders' literal copies near the end of the input, where wild_copy would read past it. A memcpy call
// there, even one that rarely runs, sits inside the parse loop and makes the compiler save and reload the loop's
// values around it on every token.
ALWAYS_INLINE void copy_exact_inline(uint8_t *destination, const uint8_t *source, size_t length) {
    if (length >= 16) {
        for (size_t done = 0; done + 16 < length; done += 16) copy16(destination + done, source + done);
        copy16(destination + length - 16, source + length - 16);
    } else if (length >= 8) {
        copy8(destination, source);
        copy8(destination + length - 8, source + length - 8);
    } else if (length >= 4) {
        store_u32(destination, load_u32(source));
        store_u32(destination + length - 4, load_u32(source + length - 4));
    } else if (length > 0) {
        destination[0] = source[0];
        destination[length / 2] = source[length / 2];
        destination[length - 1] = source[length - 1];
    }
}

// Execute one decoded token in the decoder's hot loop: copy `literal_length` literals from
// `literals + literal_position`, then `match_length` bytes from `distance` back, to `output + position`.
// One combined check stands for all the copies' bounds and the body makes no function calls, so the decoder's
// loop keeps its state in registers. Returns false, having written nothing, when the token needs the careful
// path: too close to the end of either buffer, an invalid distance, or a run longer than 64 bytes with a period
// shorter than 8.
//
// `restrict` promises the compiler that the output and the literals do not overlap, so it need not reload one
// after writing the other. `literals_capacity` is how far the literals may be read (16 past each token's literals).
//
// Callers pass small lengths, which keeps the sums below from overflowing without a check: the format's constant
// tables cap an LZFSE literal length at 315 and a match length at 2359 (whatever a corrupt stream holds), and an
// LZVN opcode carries at most 271 of either.
ALWAYS_INLINE bool decode_token_fast(uint8_t *restrict output, size_t output_capacity, size_t position,
                                     const uint8_t *restrict literals, size_t literals_capacity,
                                     size_t literal_position, size_t literal_length, size_t distance,
                                     size_t match_length) {
    size_t match_start = position + literal_length;
    // `distance - 1 < match_start` checks 1 <= distance <= match_start in one comparison: for distance 0 the
    // unsigned subtraction wraps around to the largest value.
    bool ok = match_start + match_length + COPY_SLACK <= output_capacity &&
              literal_position + literal_length + 16 <= literals_capacity && distance - 1 < match_start &&
              (distance >= 8 || match_length <= 64);
    if (!ok) return false;
    // The checks above keep every step in bounds: literal steps read at most 15 bytes past the literals and
    // write at most 15 past `match_start`; match steps write at most 15 past the match. Each match step copies at
    // most `distance` bytes, so it only reads bytes already written.
    // From here on `literals` and `output` point at this token's literals and at where its output goes.
    literals += literal_position;
    output += position;
    copy16(output, literals);
    for (size_t done = 16; done < literal_length; done += 16) copy16(output + done, literals + done);
    output += literal_length;
    const uint8_t *match_source = output - distance;
    if (distance >= 16) {
        copy16(output, match_source);
        for (size_t done = 16; done < match_length; done += 16) copy16(output + done, match_source + done);
    } else if (distance >= 8) {
        copy8(output, match_source);
        for (size_t done = 8; done < match_length; done += 8) copy8(output + done, match_source + done);
    } else {
        // A short period: write the first 8 bytes one at a time (the pattern repeats every `distance` bytes),
        // then copy 8 bytes per step from `period_copy` back, the smallest multiple of `distance` that is at
        // least 8, so each step's source is already written and does not overlap its destination.
        for (size_t i = 0; i < 8; i++) output[i] = match_source[i];
        size_t period_copy = distance * div_ceil(8, distance);
        for (size_t done = 8; done < match_length; done += 8) copy8(output + done, output + done - period_copy);
    }
    return true;
}

#endif
