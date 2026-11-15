// FSE table construction: normalizing counts, and building encoder and decoder tables.
#include "fse.h"

#include "format.h"

// The k for which state_count <= frequency * 2^k < 2 * state_count (state_count is a power of two and
// 0 < frequency <= state_count): the difference of their leading-zero counts.
static uint32_t shift_for(uint32_t frequency, uint32_t state_count) {
    return (uint32_t)(__builtin_clz(frequency) - __builtin_clz(state_count));
}

// Reorder `keys[0 .. count)` so that its `wanted` largest keys come first (0 < wanted < count), in any order.
// The keys are distinct. This is quickselect: like quicksort, it splits the range around a pivot (larger keys to
// the left), but then continues only in the part that contains position `wanted`, so it takes linear time.
static void select_largest(uint64_t *keys, size_t count, size_t wanted) {
    size_t low = 0, high = count - 1;
    while (low < high) {
        uint64_t pivot = keys[low + (high - low) / 2];
        size_t i = low, j = high;
        // Hoare partition, largest first: keys[low .. i) >= pivot >= keys(j .. high].
        while (i <= j) {
            while (keys[i] > pivot) i++;
            while (keys[j] < pivot) j--;
            if (i <= j) {
                uint64_t swap = keys[i];
                keys[i] = keys[j];
                keys[j] = swap;
                i++;
                if (j == 0) break;
                j--;
            }
        }
        if (wanted <= j) {
            high = j;
        } else if (wanted >= i) {
            low = i;
        } else {
            return;
        }
    }
}

void fse_normalize(const uint32_t *counts, size_t symbol_count, uint32_t state_count, uint16_t *frequencies) {
    uint64_t total = 0;
    for (size_t i = 0; i < symbol_count; i++) total += counts[i];
    // Not cleared up front: the loop below writes every entry (0 for unused symbols), and an empty input gets
    // its zeros here, so no entry is left unset.
    // clear_bytes(frequencies, symbol_count * sizeof *frequencies);
    if (total == 0) {
        for (size_t i = 0; i < symbol_count; i++) frequencies[i] = 0;
        return;
    }
    // Largest-remainder rounding: give every used symbol the floor of its share (at least 1), then hand the
    // leftover states to the largest remainders.
    // Each used symbol's leftover share and the symbol, packed into one key: remainder << 8 | (255 - symbol).
    // The remainder is below `total` (at most 256 counts of 32 bits, so below 2^40), so the key fits 48 bits.
    // A larger key means a larger remainder, then a lower symbol, so plain integer order is the order wanted.
    uint64_t keys[256];
    size_t used = 0;
    uint32_t assigned = 0;
    for (size_t i = 0; i < symbol_count; i++) {
        if (counts[i] == 0) {
            frequencies[i] = 0;
            continue;
        }
        uint64_t scaled = (uint64_t)counts[i] * state_count;
        uint32_t share = (uint32_t)(scaled / total);
        if (share < 1) share = 1;
        frequencies[i] = (uint16_t)share;
        assigned += share;
        keys[used++] = (scaled % total) << 8 | (255 - i);
    }
    if (assigned < state_count) {
        // Every used symbol rounded down by less than 1, so fewer than `used` states are left: they go to the
        // `left` largest keys, one each.
        uint32_t left = state_count - assigned;
        while (left >= used) {
            for (size_t r = 0; r < used; r++) frequencies[255 - (keys[r] & 0xFF)]++;
            left -= (uint32_t)used;
        }
        if (left > 0) {
            select_largest(keys, used, left);
            for (size_t r = 0; r < left; r++) frequencies[255 - (keys[r] & 0xFF)]++;
        }
    }
    // Rounding small symbols up to 1 can overshoot; take the excess from the largest frequencies.
    uint32_t excess = assigned > state_count ? assigned - state_count : 0;
    while (excess > 0) {
        size_t largest = 0;
        for (size_t i = 1; i < symbol_count; i++) {
            if (frequencies[i] > frequencies[largest]) largest = i;
        }
        if (frequencies[largest] <= 1) break;
        uint32_t take = (frequencies[largest] - 1u) / 2;
        if (take > excess) take = excess;
        if (take < 1) take = 1;
        frequencies[largest] = (uint16_t)(frequencies[largest] - take);
        excess -= take;
    }
}

void fse_encoder_table(const uint16_t *frequencies, size_t symbol_count, uint32_t state_count,
                       fse_encoder_entry *table) {
    uint32_t offset = 0;
    for (size_t s = 0; s < symbol_count; s++) {
        uint32_t f = frequencies[s];
        if (f == 0) {
            // Not cleared: a symbol with frequency 0 never occurs in the block, so the encoder never looks up
            // its entry.
            // clear_bytes(&table[s], sizeof table[s]);
            continue;
        }
        uint32_t k = shift_for(f, state_count);
        // See fse_encoder_entry in fse.h. Both values fit easily: k <= 10 and f * 2^k < 2 * state_count.
        table[s].bits_delta = (int32_t)(k << 16) - (int32_t)(f << k);
        table[s].state_delta = (int32_t)(offset + state_count) - (int32_t)f;
        offset += f;
    }
}

// How the states of one symbol split into two runs. A symbol with frequency `f` owns `f` consecutive states. The
// first `high_count` read `k` bits and their next-state bases rise by 2^k per state from `high_base`; the rest
// read `k - 1` bits and their bases rise by 2^(k-1) from 0. Within each run the bits are the same and the base
// grows by a fixed step, so the table builders fill each run with a loop that has no branch, which the compiler
// turns into vector stores of several entries per step.
typedef struct state_runs {
    uint32_t k;
    uint32_t high_count;
    uint32_t high_base;
} state_runs;

// Describe the states of a symbol with frequency `f` (0 < f <= state_count) in a table of `state_count`.
ALWAYS_INLINE state_runs state_runs_for(uint32_t f, uint32_t state_count) {
    uint32_t k = shift_for(f, state_count);
    state_runs runs;
    runs.k = k;
    // At most f: when k is 0 (f == state_count) it is exactly f, so the second run is empty and k - 1 is unused.
    runs.high_count = ((2 * state_count) >> k) - f;
    runs.high_base = (f << k) - state_count;
    return runs;
}

// Write `count` literal entries for `symbol` that read `bits` bits, with next-state bases `first_base`,
// `first_base + step`, `first_base + 2 * step` and so on. The fields are assigned one by one: the compiler then
// writes 4 entries per vector store, which it does not do for a whole-struct assignment or a copy_bytes (memcpy).
ALWAYS_INLINE void fill_literal_run(fse_literal_entry *entries, uint32_t count, uint32_t bits, uint32_t symbol,
                                    uint32_t first_base, uint32_t step) {
    for (uint32_t j = 0; j < count; j++) {
        entries[j].bits = (uint8_t)bits;
        entries[j].symbol = (uint8_t)symbol;
        entries[j].next_state_base = (uint16_t)(first_base + j * step);
    }
}

// Write `count` value entries that read `bits` state bits plus `extra` value bits and decode to `value_base` plus
// the value bits, with next-state bases `first_base`, `first_base + step` and so on (fields assigned one by one,
// as in fill_literal_run).
ALWAYS_INLINE void fill_value_run(fse_value_entry *entries, uint32_t count, uint32_t bits, uint32_t extra,
                                  uint32_t value_base, uint32_t first_base, uint32_t step) {
    for (uint32_t j = 0; j < count; j++) {
        entries[j].total_bits = (uint8_t)(bits + extra);
        entries[j].value_bits = (uint8_t)extra;
        entries[j].next_state_base = (uint16_t)(first_base + j * step);
        entries[j].value_base = value_base;
    }
}

lzfse_zazzcat_status fse_literal_table(const uint16_t *frequencies, uint32_t state_count, fse_literal_entry *table) {
    // Not cleared up front: the loop below writes every state from 0 up to the sum of the frequencies, and only
    // the states past that sum are cleared at the end. Untrusted streams may have frequencies that sum to less
    // than state_count, and the decoder can still reach those states, so they must hold a defined (zero) entry.
    // A table left half-built by a CORRUPT return is never used.
    // clear_bytes(table, state_count * sizeof *table);
    uint32_t offset = 0;
    for (uint32_t symbol = 0; symbol < LITERAL_SYMBOLS; symbol++) {
        uint32_t f = frequencies[symbol];
        if (f == 0) continue;
        if (offset + f > state_count) return LZFSE_ZAZZCAT_CORRUPT;
        state_runs runs = state_runs_for(f, state_count);
        // The second call's shift by k - 1 only happens when its count is not zero, and then k >= 1.
        fill_literal_run(table + offset, runs.high_count, runs.k, symbol, runs.high_base, 1u << runs.k);
        if (runs.high_count < f) {
            fill_literal_run(table + offset + runs.high_count, f - runs.high_count, runs.k - 1, symbol, 0,
                             1u << (runs.k - 1));
        }
        offset += f;
    }
    // States the frequencies did not cover (none for well-formed streams, whose frequencies sum to state_count).
    clear_bytes(table + offset, (state_count - offset) * sizeof *table);
    return LZFSE_ZAZZCAT_OK;
}

lzfse_zazzcat_status fse_value_table(const uint16_t *frequencies, size_t symbol_count, uint32_t state_count,
                                     const uint8_t *extra_bits, const uint32_t *value_base, fse_value_entry *table) {
    // Not cleared up front, for the same reason as in fse_literal_table: every state up to the sum of the
    // frequencies is written below, and only the states past it are cleared at the end.
    // clear_bytes(table, state_count * sizeof *table);
    uint32_t offset = 0;
    for (size_t symbol = 0; symbol < symbol_count; symbol++) {
        uint32_t f = frequencies[symbol];
        if (f == 0) continue;
        if (offset + f > state_count) return LZFSE_ZAZZCAT_CORRUPT;
        state_runs runs = state_runs_for(f, state_count);
        uint32_t extra = extra_bits[symbol];
        fill_value_run(table + offset, runs.high_count, runs.k, extra, value_base[symbol], runs.high_base,
                       1u << runs.k);
        if (runs.high_count < f) {
            fill_value_run(table + offset + runs.high_count, f - runs.high_count, runs.k - 1, extra, value_base[symbol],
                           0, 1u << (runs.k - 1));
        }
        offset += f;
    }
    // States the frequencies did not cover (none for well-formed streams, whose frequencies sum to state_count).
    clear_bytes(table + offset, (state_count - offset) * sizeof *table);
    return LZFSE_ZAZZCAT_OK;
}
