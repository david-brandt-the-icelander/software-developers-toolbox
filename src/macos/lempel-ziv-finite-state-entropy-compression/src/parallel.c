// Parallel compression of independent chunks (each chunk becomes its own complete stream) on a persistent pool of
// threads. Consecutive chunks are grouped into a few tasks per thread; each task encodes its chunks with its
// thread's own encoder into one buffer reserved up front. The buffers, the per-chunk end offsets and the encoders
// persist between calls, so a warmed-up call allocates nothing.
//
// Build with LZFSE_ZAZZCAT_NO_THREADS to get a serial version of the same API (for platforms without pthreads).
#include "bare_functions.h"
#include "buffer.h"
#include "common.h"

// Tasks per thread: enough for load balancing, few enough that each task's buffer is large.
#define TASKS_PER_THREAD 4

// Room to reserve for encoding `length` bytes as `chunks` streams: the encoder may briefly hold a block coded at
// up to 10 bits per literal (plus its header) before deciding to store it raw.
static size_t task_bound(size_t length, size_t chunks) {
    return length + length / 4 + chunks * 4096;
}

// How one call's input is cut up: `chunk_count` chunks of `chunk_size` bytes (the last may be shorter), grouped
// into `task_count` tasks of `chunks_per_task` consecutive chunks (the last task may have fewer).
typedef struct chunk_layout {
    const uint8_t *source;
    size_t length;
    size_t chunk_size;
    size_t chunk_count;
    size_t chunks_per_task;
    size_t task_count;
} chunk_layout;

// Working arrays kept between calls, with their capacities: one output buffer per task, and where each chunk's
// stream ends within its task's buffer.
typedef struct task_scratch {
    lzfse_zazzcat_buffer *task_buffers;
    size_t task_buffer_capacity;
    size_t *ends;
    size_t ends_capacity;
} task_scratch;

// Encode the chunks of task number `task` into that task's buffer, recording where each chunk's stream ends.
static lzfse_zazzcat_status encode_task(lzfse_zazzcat_encoder *encoder, const chunk_layout *layout, size_t task,
                                        task_scratch *scratch) {
    // Read the layout once: it is shared with other threads, and next to fields they keep writing (see `job`).
    const uint8_t *source = layout->source;
    size_t length = layout->length, chunk_size = layout->chunk_size;
    size_t first_chunk = task * layout->chunks_per_task;
    size_t chunk_count = min_size(layout->chunks_per_task, layout->chunk_count - first_chunk);
    size_t start = first_chunk * chunk_size;
    size_t task_length = min_size(chunk_count * chunk_size, length - start);
    size_t *ends = scratch->ends + first_chunk;
    // Work on a copy of the buffer's fields on this thread's stack. The encoder updates the length every few
    // bytes; in the shared array, neighbouring tasks' buffers share a cache line, and those stores from different
    // cores would keep taking the line from each other ("false sharing").
    lzfse_zazzcat_buffer local = scratch->task_buffers[task];
    local.length = 0;
    lzfse_zazzcat_status status = LZFSE_ZAZZCAT_OK;
    if (!buffer_reserve(&local, task_bound(task_length, chunk_count))) status = LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    for (size_t c = 0; c < chunk_count && status == LZFSE_ZAZZCAT_OK; c++) {
        size_t at = start + c * chunk_size;
        status = lzfse_zazzcat_encoder_encode(encoder, source + at, min_size(chunk_size, length - at), &local);
        ends[c] = local.length;
    }
    scratch->task_buffers[task] = local;
    return status;
}

// Grow an offsets array to hold at least `capacity` entries.
static bool offsets_reserve(lzfse_zazzcat_offsets *offsets, size_t capacity) {
    if (offsets->capacity >= capacity) return true;
    size_t *grown = zz_realloc(offsets->data, capacity * sizeof *grown);
    if (grown == NULL) return false;
    offsets->data = grown;
    offsets->capacity = capacity;
    return true;
}

void lzfse_zazzcat_offsets_free(lzfse_zazzcat_offsets *offsets) {
    zz_free(offsets->data);
    offsets->data = NULL;
    offsets->length = 0;
    offsets->capacity = 0;
}

// Copy the tasks' buffers to `output` back to back and fill `offsets` with every stream's start and the end.
static lzfse_zazzcat_status gather(const chunk_layout *layout, const task_scratch *scratch,
                                   lzfse_zazzcat_buffer *output, lzfse_zazzcat_offsets *offsets) {
    const lzfse_zazzcat_buffer *task_buffers = scratch->task_buffers;
    size_t total = 0;
    for (size_t t = 0; t < layout->task_count; t++) total += task_buffers[t].length;
    if (!buffer_reserve(output, total) || !offsets_reserve(offsets, layout->chunk_count + 1)) {
        return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    }
    offsets->data[0] = output->length;
    offsets->length = 1;
    for (size_t t = 0; t < layout->task_count; t++) {
        size_t at = output->length;
        buffer_append_reserved(output, task_buffers[t].data, task_buffers[t].length);
        size_t first = t * layout->chunks_per_task;
        size_t count = min_size(layout->chunks_per_task, layout->chunk_count - first);
        for (size_t c = 0; c < count; c++) offsets->data[offsets->length++] = at + scratch->ends[first + c];
    }
    return LZFSE_ZAZZCAT_OK;
}

// Make sure the scratch arrays hold one buffer per task and one end per chunk of `layout`.
static bool scratch_reserve(task_scratch *scratch, const chunk_layout *layout) {
    if (scratch->task_buffer_capacity < layout->task_count) {
        lzfse_zazzcat_buffer *grown = zz_realloc(scratch->task_buffers, layout->task_count * sizeof *grown);
        if (grown == NULL) return false;
        // New entries start as empty buffers.
        clear_bytes(grown + scratch->task_buffer_capacity,
                    (layout->task_count - scratch->task_buffer_capacity) * sizeof *grown);
        scratch->task_buffers = grown;
        scratch->task_buffer_capacity = layout->task_count;
    }
    if (scratch->ends_capacity < layout->chunk_count) {
        size_t *grown = zz_realloc(scratch->ends, layout->chunk_count * sizeof *grown);
        if (grown == NULL) return false;
        scratch->ends = grown;
        scratch->ends_capacity = layout->chunk_count;
    }
    return true;
}

#ifdef LZFSE_ZAZZCAT_NO_THREADS

// Serial version: one encoder and one set of scratch arrays, kept between calls (not safe to call from two
// threads).
static lzfse_zazzcat_encoder *serial_encoder;
static task_scratch serial_scratch;

size_t lzfse_zazzcat_parallelism(void) {
    return 1;
}

lzfse_zazzcat_status lzfse_zazzcat_encode_chunks(const void *source, size_t length, size_t chunk_size,
                                                 const lzfse_zazzcat_options *options, lzfse_zazzcat_buffer *output,
                                                 lzfse_zazzcat_offsets *offsets) {
    if (chunk_size == 0) return LZFSE_ZAZZCAT_INVALID_ARGUMENT;
    offsets->length = 0;
    size_t chunk_count = div_ceil(length, chunk_size);
    if (!offsets_reserve(offsets, chunk_count + 1)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    offsets->data[offsets->length++] = output->length;
    if (chunk_count == 0) return LZFSE_ZAZZCAT_OK;
    if (serial_encoder == NULL && (serial_encoder = lzfse_zazzcat_encoder_create(options)) == NULL) {
        return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    }
    lzfse_zazzcat_encoder_set_options(serial_encoder, options);
    // One task holding every chunk.
    chunk_layout layout = {
        .source = source,
        .length = length,
        .chunk_size = chunk_size,
        .chunk_count = chunk_count,
        .chunks_per_task = chunk_count,
        .task_count = 1,
    };
    if (!scratch_reserve(&serial_scratch, &layout)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    lzfse_zazzcat_status status = encode_task(serial_encoder, &layout, 0, &serial_scratch);
    if (status != LZFSE_ZAZZCAT_OK) return status;
    return gather(&layout, &serial_scratch, output, offsets);
}

#else

#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>

// One call's work, shared by all threads while it runs.
typedef struct job {
    chunk_layout layout;
    lzfse_zazzcat_options options;
    // The pool's scratch arrays; each task writes only its own buffer and its own chunks' ends.
    task_scratch *scratch;
    // Next task index to claim (threads take tasks until they run out), and the first error.
    atomic_size_t next_task;
    atomic_int status;
} job;

// The persistent pool. Thread 0 is whichever thread calls lzfse_zazzcat_encode_chunks; threads 1.. are workers.
typedef struct worker_pool {
    size_t thread_count;
    // One encoder per thread, created on first use and kept.
    lzfse_zazzcat_encoder **encoders;
    // Serializes calls: one job runs at a time.
    pthread_mutex_t call_mutex;
    // Guards the fields below; workers sleep on `job_ready` and the caller on `job_finished`.
    pthread_mutex_t mutex;
    pthread_cond_t job_ready;
    pthread_cond_t job_finished;
    job *current_job;
    // Incremented for every job, so a worker can tell a new job from one it has already done.
    uint64_t generation;
    // Workers that have finished the current job.
    size_t finished_workers;
    // Task buffers and chunk ends, kept between calls (guarded by call_mutex).
    task_scratch scratch;
} worker_pool;

static worker_pool pool;
static pthread_once_t pool_once = PTHREAD_ONCE_INIT;

// Claim and run tasks of `j` with thread `thread_index`'s encoder until none are left.
static void run_tasks(job *j, size_t thread_index) {
    lzfse_zazzcat_encoder **encoder = &pool.encoders[thread_index];
    for (;;) {
        size_t task = atomic_fetch_add(&j->next_task, 1);
        if (task >= j->layout.task_count) return;
        lzfse_zazzcat_status status = LZFSE_ZAZZCAT_OK;
        if (*encoder == NULL) *encoder = lzfse_zazzcat_encoder_create(&j->options);
        if (*encoder == NULL) {
            status = LZFSE_ZAZZCAT_OUT_OF_MEMORY;
        } else {
            lzfse_zazzcat_encoder_set_options(*encoder, &j->options);
            status = encode_task(*encoder, &j->layout, task, j->scratch);
        }
        if (status != LZFSE_ZAZZCAT_OK) {
            int ok = LZFSE_ZAZZCAT_OK;
            atomic_compare_exchange_strong(&j->status, &ok, (int)status);
        }
    }
}

// A worker thread: wait for each new job, help with its tasks, report that it is done.
static void *worker_main(void *argument) {
    size_t thread_index = (size_t)(uintptr_t)argument;
    uint64_t seen_generation = 0;
    for (;;) {
        bare_pthread_mutex_lock(&pool.mutex);
        while (pool.generation == seen_generation) bare_pthread_cond_wait(&pool.job_ready, &pool.mutex);
        seen_generation = pool.generation;
        job *j = pool.current_job;
        bare_pthread_mutex_unlock(&pool.mutex);
        run_tasks(j, thread_index);
        bare_pthread_mutex_lock(&pool.mutex);
        if (++pool.finished_workers == pool.thread_count - 1) bare_pthread_cond_signal(&pool.job_finished);
        bare_pthread_mutex_unlock(&pool.mutex);
    }
    return NULL;
}

// Start the pool: one thread per online CPU (counting the caller). If a thread cannot be created, the pool simply
// has fewer threads.
static void pool_start(void) {
    long cpus = sysconf(_SC_NPROCESSORS_ONLN);
    size_t wanted = cpus > 0 ? (size_t)cpus : 1;
    bare_pthread_mutex_init(&pool.call_mutex, NULL);
    bare_pthread_mutex_init(&pool.mutex, NULL);
    bare_pthread_cond_init(&pool.job_ready, NULL);
    bare_pthread_cond_init(&pool.job_finished, NULL);
    pool.encoders = zz_calloc(wanted, sizeof *pool.encoders);
    pool.thread_count = 1;
    if (pool.encoders == NULL) return;
    for (size_t i = 1; i < wanted; i++) {
        pthread_t thread;
        if (pthread_create(&thread, NULL, worker_main, (void *)(uintptr_t)i) != 0) break;
        bare_pthread_detach(thread);
        pool.thread_count++;
    }
}

size_t lzfse_zazzcat_parallelism(void) {
    bare_pthread_once(&pool_once, pool_start);
    return pool.thread_count;
}

lzfse_zazzcat_status lzfse_zazzcat_encode_chunks(const void *source, size_t length, size_t chunk_size,
                                                 const lzfse_zazzcat_options *options, lzfse_zazzcat_buffer *output,
                                                 lzfse_zazzcat_offsets *offsets) {
    if (chunk_size == 0) return LZFSE_ZAZZCAT_INVALID_ARGUMENT;
    offsets->length = 0;
    size_t chunk_count = div_ceil(length, chunk_size);
    if (chunk_count == 0) {
        if (!offsets_reserve(offsets, 1)) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
        offsets->data[offsets->length++] = output->length;
        return LZFSE_ZAZZCAT_OK;
    }
    size_t threads = lzfse_zazzcat_parallelism();
    if (pool.encoders == NULL) return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    size_t chunks_per_task = div_ceil(chunk_count, threads * TASKS_PER_THREAD);
    job j = {
        .layout =
            {
                .source = source,
                .length = length,
                .chunk_size = chunk_size,
                .chunk_count = chunk_count,
                .chunks_per_task = chunks_per_task,
                .task_count = div_ceil(chunk_count, chunks_per_task),
            },
        .options = options != NULL ? *options : lzfse_zazzcat_default_options(),
        .scratch = &pool.scratch,
    };
    atomic_init(&j.next_task, 0);
    atomic_init(&j.status, LZFSE_ZAZZCAT_OK);

    bare_pthread_mutex_lock(&pool.call_mutex);
    if (!scratch_reserve(&pool.scratch, &j.layout)) {
        bare_pthread_mutex_unlock(&pool.call_mutex);
        return LZFSE_ZAZZCAT_OUT_OF_MEMORY;
    }
    // Wake the workers only when there is more than one task for them to share.
    bool use_workers = pool.thread_count > 1 && j.layout.task_count > 1;
    if (use_workers) {
        pthread_mutex_lock(&pool.mutex);
        pool.current_job = &j;
        pool.finished_workers = 0;
        pool.generation++;
        pthread_cond_broadcast(&pool.job_ready);
        pthread_mutex_unlock(&pool.mutex);
    }
    run_tasks(&j, 0);
    if (use_workers) {
        // Wait until every worker has stopped touching the job (it lives on this function's stack).
        pthread_mutex_lock(&pool.mutex);
        while (pool.finished_workers < pool.thread_count - 1) pthread_cond_wait(&pool.job_finished, &pool.mutex);
        pool.current_job = NULL;
        bare_pthread_mutex_unlock(&pool.mutex);
    }
    lzfse_zazzcat_status status = (lzfse_zazzcat_status)atomic_load(&j.status);
    if (status == LZFSE_ZAZZCAT_OK) {
        status = gather(&j.layout, &pool.scratch, output, offsets);
    }
    bare_pthread_mutex_unlock(&pool.call_mutex);
    return status;
}

#endif
