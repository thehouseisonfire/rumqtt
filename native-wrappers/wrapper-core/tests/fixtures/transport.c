/* Private static fixture. All callbacks and views are test-only contracts. */
#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <stdatomic.h>
#endif

enum { READ = 0, WRITE = 1, FLUSH = 2, SHUTDOWN = 3, CONNECT = 4, KINDS = 5 };
typedef uint32_t (*bytes_done_fn)(void *, const uint8_t *, size_t, uint32_t);
typedef uint32_t (*count_done_fn)(void *, size_t, uint32_t);
typedef uint32_t (*unit_done_fn)(void *, uint32_t);
typedef void (*release_fn)(void *);

typedef struct pending {
    void *token;
    size_t count;
    const uint8_t *input;
    bytes_done_fn bytes_done;
    count_done_fn count_done;
    unit_done_fn unit_done;
    release_fn release;
} pending;

typedef struct fixture {
#ifdef _WIN32
    SRWLOCK lock;
#else
    atomic_flag lock;
#endif
    uint8_t incoming[65536];
    size_t incoming_len;
    uint8_t outgoing[65536];
    size_t outgoing_len;
    size_t chunk;
    uint32_t deferred;
    int eof;
    pending operations[KINDS];
    unsigned starts[KINDS];
    unsigned max_read;
    unsigned max_write;
    uint32_t first_status;
    uint32_t last_status;
    release_fn before_read_completion;
    void *before_read_context;
} fixture;

typedef struct completion {
    pending operation;
    size_t count;
    uint8_t data[16384];
    release_fn before_read;
    void *before_read_context;
} completion;

static void acquire(fixture *f) {
#ifdef _WIN32
    AcquireSRWLockExclusive(&f->lock);
#else
    while (atomic_flag_test_and_set_explicit(&f->lock, memory_order_acquire)) {}
#endif
}

static void unlock(fixture *f) {
#ifdef _WIN32
    ReleaseSRWLockExclusive(&f->lock);
#else
    atomic_flag_clear_explicit(&f->lock, memory_order_release);
#endif
}

fixture *proof_new(size_t chunk, uint32_t deferred) {
    if (chunk == 0 || chunk > 16384) return NULL;
    fixture *f = calloc(1, sizeof(*f));
    if (f == NULL) return NULL;
#ifdef _WIN32
    InitializeSRWLock(&f->lock);
#else
    atomic_flag_clear(&f->lock);
#endif
    f->chunk = chunk;
    f->deferred = deferred;
    return f;
}

void proof_destroy(fixture *f) {
    for (unsigned kind = 0; kind < KINDS; ++kind) assert(f->operations[kind].token == NULL);
    free(f);
}

static size_t take_input(fixture *f, uint8_t *output, size_t max) {
    size_t count = f->incoming_len;
    if (count > max) count = max;
    if (count > f->chunk) count = f->chunk;
    memcpy(output, f->incoming, count);
    memmove(f->incoming, f->incoming + count, f->incoming_len - count);
    f->incoming_len -= count;
    return count;
}

static void install(fixture *f, unsigned kind, pending operation) {
    assert(kind < KINDS && f->operations[kind].token == NULL);
    if (kind >= WRITE && kind <= SHUTDOWN) {
        for (unsigned other = WRITE; other <= SHUTDOWN; ++other) assert(f->operations[other].token == NULL);
    }
    f->operations[kind] = operation;
    ++f->starts[kind];
}

/* Called with the lock held: claim the operation and capture its result in
 * the same critical section as the readiness check. Delivery never looks up
 * the slot again, even if another thread has already installed its next read. */
static void prepare_completion(fixture *f, unsigned kind, completion *completed) {
    assert(kind < KINDS);
    pending operation = f->operations[kind];
    completed->operation = operation;
    if (operation.token == NULL) return;
    memset(&f->operations[kind], 0, sizeof(pending));
    size_t natural = operation.count;
    if (kind == READ) {
        natural = take_input(f, completed->data, operation.count);
        completed->before_read = f->before_read_completion;
        completed->before_read_context = f->before_read_context;
        f->before_read_completion = NULL;
        f->before_read_context = NULL;
    }
    if (kind == WRITE) {
        assert(natural <= sizeof(f->outgoing) - f->outgoing_len);
        memcpy(f->outgoing + f->outgoing_len, operation.input, natural);
        f->outgoing_len += natural;
    }
    completed->count = natural;
}

static void prepare_ready_read(fixture *f, completion *completed) {
    if ((f->incoming_len > 0 || f->eof) && !(f->deferred & (1u << READ))) {
        prepare_completion(f, READ, completed);
    }
}

/* All hooks, completions, and releases run outside the fixture lock. */
static void deliver_completion(fixture *f, unsigned kind, completion *completed,
                               uint32_t result, size_t count, int duplicate) {
    pending operation = completed->operation;
    if (operation.token == NULL) return;
    if (completed->before_read != NULL) completed->before_read(completed->before_read_context);
    if (count == SIZE_MAX) count = completed->count;
    uint32_t first;
    uint32_t last;
    if (kind == READ) {
        first = operation.bytes_done(operation.token, completed->data, count, result);
        last = duplicate ? operation.bytes_done(operation.token, completed->data, count, result) : first;
    } else if (kind == WRITE) {
        first = operation.count_done(operation.token, count, result);
        last = duplicate ? operation.count_done(operation.token, count, result) : first;
    } else {
        first = operation.unit_done(operation.token, result);
        last = duplicate ? operation.unit_done(operation.token, result) : first;
    }
    acquire(f);
    f->first_status = first;
    f->last_status = last;
    unlock(f);
    operation.release(operation.token);
}

/* SIZE_MAX uses the natural short-transfer count. A supplied count lets tests
 * exercise malformed completions without dereferencing invalid buffers. */
void proof_finish(fixture *f, unsigned kind, uint32_t result, size_t count, int duplicate) {
    completion completed = {0};
    acquire(f);
    prepare_completion(f, kind, &completed);
    unlock(f);
    deliver_completion(f, kind, &completed, result, count, duplicate);
}

/* One-shot scheduling hook for deterministic read-completion race tests.
 * The caller keeps context alive until that completion has returned. */
void proof_before_read_completion(fixture *f, release_fn hook, void *context) {
    acquire(f);
    assert(f->before_read_completion == NULL);
    f->before_read_completion = hook;
    f->before_read_context = context;
    unlock(f);
}

void proof_read(fixture *f, void *token, size_t max, bytes_done_fn done, release_fn release) {
    completion completed = {0};
    pending operation = {0};
    operation.token = token;
    operation.count = max;
    operation.bytes_done = done;
    operation.release = release;
    acquire(f);
    assert(max > 0 && max <= 16384);
    install(f, READ, operation);
    if (max > f->max_read) f->max_read = (unsigned)max;
    prepare_ready_read(f, &completed);
    unlock(f);
    deliver_completion(f, READ, &completed, 0, SIZE_MAX, 0);
}

void proof_write(fixture *f, void *token, const uint8_t *data, size_t length,
                 count_done_fn done, release_fn release) {
    completion completed = {0};
    pending operation = {0};
    operation.token = token;
    operation.count_done = done;
    operation.release = release;
    acquire(f);
    assert(length > 0 && length <= 16384);
    size_t count = length < f->chunk ? length : f->chunk;
    operation.count = count;
    operation.input = data;
    install(f, WRITE, operation);
    if (length > f->max_write) f->max_write = (unsigned)length;
    if (!(f->deferred & (1u << WRITE))) prepare_completion(f, WRITE, &completed);
    unlock(f);
    deliver_completion(f, WRITE, &completed, 0, SIZE_MAX, 0);
}

void proof_unit(fixture *f, unsigned kind, void *token, unit_done_fn done, release_fn release) {
    completion completed = {0};
    pending operation = {0};
    operation.token = token;
    operation.unit_done = done;
    operation.release = release;
    acquire(f);
    assert(kind >= FLUSH && kind < KINDS);
    install(f, kind, operation);
    if (!(f->deferred & (1u << kind))) prepare_completion(f, kind, &completed);
    unlock(f);
    deliver_completion(f, kind, &completed, 0, SIZE_MAX, 0);
}

void proof_feed(fixture *f, const uint8_t *data, size_t length) {
    completion completed = {0};
    acquire(f);
    assert(length <= sizeof(f->incoming) - f->incoming_len);
    if (length > 0) memcpy(f->incoming + f->incoming_len, data, length);
    f->incoming_len += length;
    prepare_ready_read(f, &completed);
    unlock(f);
    deliver_completion(f, READ, &completed, 0, SIZE_MAX, 0);
}

void proof_eof(fixture *f) {
    completion completed = {0};
    acquire(f);
    f->eof = 1;
    prepare_ready_read(f, &completed);
    unlock(f);
    deliver_completion(f, READ, &completed, 0, SIZE_MAX, 0);
}

/* Cancellation never performs foreign I/O. The host explicitly releases all
 * detached work when it is finished, even when the observer has disappeared. */
void proof_release_pending(fixture *f) {
    pending released[KINDS];
    acquire(f);
    memcpy(released, f->operations, sizeof(released));
    memset(f->operations, 0, sizeof(f->operations));
    unlock(f);
    for (unsigned kind = 0; kind < KINDS; ++kind) {
        if (released[kind].token != NULL) released[kind].release(released[kind].token);
    }
}

unsigned proof_pending(fixture *f, unsigned kind) {
    acquire(f);
    assert(kind < KINDS);
    unsigned result = f->operations[kind].token != NULL;
    unlock(f);
    return result;
}

unsigned proof_starts(fixture *f, unsigned kind) {
    acquire(f);
    assert(kind < KINDS);
    unsigned result = f->starts[kind];
    unlock(f);
    return result;
}

uint32_t proof_status(fixture *f, int first) {
    acquire(f);
    uint32_t result = first ? f->first_status : f->last_status;
    unlock(f);
    return result;
}

unsigned proof_max_transfer(fixture *f, int read) {
    acquire(f);
    unsigned result = read ? f->max_read : f->max_write;
    unlock(f);
    return result;
}

size_t proof_outgoing(fixture *f, uint8_t *output, size_t capacity) {
    acquire(f);
    size_t count = f->outgoing_len < capacity ? f->outgoing_len : capacity;
    if (count > 0) memcpy(output, f->outgoing, count);
    unlock(f);
    return count;
}
