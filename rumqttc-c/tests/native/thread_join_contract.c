#include "example_common.h"

#include <stdatomic.h>

typedef struct worker_state {
  _Atomic int released;
} worker_state_t;

static int wait_for_release(void *opaque) {
  worker_state_t *state = opaque;
  while (atomic_load_explicit(&state->released, memory_order_acquire) == 0) {
  }
  return 17;
}

int main(void) {
  worker_state_t state = {0};
  example_thread_t *thread = example_thread_start(wait_for_release, &state);
  int worker_result = 0;
  if (thread == NULL)
    return 1;

  /* A timed-out join retains the thread handle for a later retry. */
  if (example_thread_join(thread, 0, &worker_result) == 0)
    return 2;

  atomic_store_explicit(&state.released, 1, memory_order_release);
  if (example_thread_join(thread, 5000, &worker_result) != 0)
    return 3;
  return worker_result == 17 ? 0 : 4;
}
