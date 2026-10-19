/* Real C consumer, also builds against the pre-readiness ABI. */
#include "native_common.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inttypes.h>
#if defined(_WIN32)
#include <windows.h>
#else
#include <errno.h>
#include <poll.h>
#include <time.h>
#include <sys/resource.h>
#endif
#ifndef WITH_READINESS
#define WITH_READINESS 0
#endif

static uint64_t now_ns(void) {
#if defined(_WIN32)
  LARGE_INTEGER value, frequency;
  REQUIRE(QueryPerformanceCounter(&value) && QueryPerformanceFrequency(&frequency));
  return (uint64_t)((double)value.QuadPart * 1000000000.0 / (double)frequency.QuadPart);
#else
  struct timespec value;
  REQUIRE(clock_gettime(CLOCK_MONOTONIC, &value) == 0);
  return (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec;
#endif
}
static void drain_budget(rumqttc_client_t *client, size_t budget) {
  for (size_t i = 0; i < budget; ++i) {
    rumqttc_event_t *event = NULL;
    rumqttc_status_t status = rumqttc_client_event_try_recv(client, &event, NULL);
    if (status == RUMQTTC_WOULD_BLOCK) return;
    CHECK(status);
    rumqttc_event_destroy(event);
  }
}
static void drain(rumqttc_client_t *client) { drain_budget(client, SIZE_MAX); }

static uint64_t cpu_ns(void) {
#if defined(_WIN32)
  FILETIME created, exited, kernel, user;
  ULARGE_INTEGER k, u;
  REQUIRE(GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user));
  k.LowPart = kernel.dwLowDateTime; k.HighPart = kernel.dwHighDateTime;
  u.LowPart = user.dwLowDateTime; u.HighPart = user.dwHighDateTime;
  return (k.QuadPart + u.QuadPart) * UINT64_C(100);
#else
  struct rusage usage;
  REQUIRE(getrusage(RUSAGE_SELF, &usage) == 0);
  return (uint64_t)(usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * UINT64_C(1000000000)
      + (uint64_t)(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) * UINT64_C(1000);
#endif
}

static void host_work(unsigned microseconds) {
  if (microseconds != 0) {
    uint64_t until = now_ns() + (uint64_t)microseconds * 1000;
    while (now_ns() < until) { /* Simulated unrelated host computation; no MQTT polling. */ }
  }
}
#if WITH_READINESS
static void wait_ready(rumqttc_notification_context_t *context, uint32_t timeout_ms) {
#if defined(_WIN32)
  void *handle = NULL;
  CHECK(rumqttc_notification_borrowed_windows_handle(context, &handle, NULL));
  DWORD result = WaitForSingleObject((HANDLE)handle, timeout_ms);
  REQUIRE(result == WAIT_OBJECT_0 || result == WAIT_TIMEOUT);
#else
  int32_t fd;
  CHECK(rumqttc_notification_borrowed_posix_fd(context, &fd, NULL));
  struct pollfd descriptor = {fd, POLLIN, 0};
  int result;
  do { result = poll(&descriptor, 1, (int)timeout_ms); } while (result < 0 && errno == EINTR);
  REQUIRE(result >= 0);
#endif
}
#endif
int main(int argc, char **argv) {
  REQUIRE(argc >= 5);
  int reactor = strcmp(argv[1], "reactor") == 0;
  int active = strcmp(argv[1], "active") == 0 || reactor;
  int blocking = strcmp(argv[1], "blocking") == 0;
  REQUIRE(active || blocking || strcmp(argv[1], "poll") == 0);
  unsigned count = (unsigned)strtoul(argv[2], NULL, 10);
  unsigned rounds = (unsigned)strtoul(argv[3], NULL, 10);
  unsigned protocol = (unsigned)strtoul(argv[4], NULL, 10);
  REQUIRE(count > 0 && count <= 1024 && rounds > 0 && rounds <= 10000);
  REQUIRE(protocol == 1 || protocol == 2);
  size_t capacity = (size_t)count * 2;
  unsigned work_us = 0;
  for (int argument = 5; argument < argc; argument += 2) {
    REQUIRE(argument + 1 < argc);
    char *end = NULL;
    unsigned long value = strtoul(argv[argument + 1], &end, 10);
    REQUIRE(end != argv[argument + 1] && *end == '\0');
    if (strcmp(argv[argument], "--capacity") == 0) {
      REQUIRE(value >= (size_t)count * 2 && value <= 1048576);
      capacity = (size_t)value;
    } else {
      REQUIRE(strcmp(argv[argument], "--host-work-us") == 0 && value <= 1000000);
      work_us = (unsigned)value;
    }
  }
#if !WITH_READINESS
  (void)capacity;
#endif
  rumqttc_execution_context_t *execution = NULL;
  rumqttc_client_t **clients = calloc(count, sizeof(*clients));
  rumqttc_completion_t **completions = calloc(count, sizeof(*completions));
  uint64_t *started = calloc(count, sizeof(*started));
  uint64_t *latencies = calloc((size_t)count * rounds, sizeof(*latencies));
  REQUIRE(clients && completions && started && latencies);
#if WITH_READINESS
  rumqttc_notification_context_t *context = NULL;
  rumqttc_notification_options_t notification_options = RUMQTTC_NOTIFICATION_OPTIONS_INIT;
  rumqttc_notification_key_t *keys = calloc(count, sizeof(*keys));
  REQUIRE(keys != NULL);
  if (active) {
    notification_options.capacity = capacity;
    CHECK(rumqttc_notification_context_new(&notification_options, &context, NULL));
  }
#else
  REQUIRE(!active);
#endif
  CHECK(rumqttc_execution_context_new(NULL, &execution, NULL));
  for (unsigned i = 0; i < count; ++i) {
    rumqttc_config_t *config = NULL;
    char id[64];
    REQUIRE(snprintf(id, sizeof(id), "readiness-bench-%u", i) > 0);
    CHECK(rumqttc_config_new(protocol, &config, NULL));
    CHECK(rumqttc_config_set_broker(config, native_string("127.0.0.1"), native_test_port(), NULL));
    CHECK(rumqttc_config_set_client_id(config, native_string(id), NULL));
    CHECK(rumqttc_config_set_tcp_nodelay(config, 1, NULL));
    CHECK(rumqttc_config_set_execution_context(config, execution, NULL));
    CHECK(rumqttc_client_start(config, &clients[i], NULL));
    rumqttc_config_destroy(config);
    rumqttc_event_destroy(native_wait_event(clients[i], RUMQTTC_EVENT_CONNECTED));
#if WITH_READINESS
    if (active) CHECK(rumqttc_notification_register_events(context, clients[i], i, &keys[i], NULL));
#endif
  }
  /* Let the runner sample idle CPU/resources during an otherwise quiet host interval. */
  puts("IDLE"); fflush(stdout);
  uint64_t idle_started = now_ns();
  uint64_t idle_cpu_started = cpu_ns();
  unsigned idle_waits = 0;
  while (now_ns() - idle_started < UINT64_C(500000000)) {
#if WITH_READINESS
    if (active) {
      rumqttc_notification_ready_t entries[32];
      size_t observed = 0;
      rumqttc_status_t status = rumqttc_notification_collect(context, entries, sizeof(entries[0]), 32, &observed, NULL);
      REQUIRE(status == RUMQTTC_OK || status == RUMQTTC_WOULD_BLOCK);
      for (size_t j = 0; j < observed; ++j) {
        drain(clients[entries[j].tag]);
        CHECK(rumqttc_notification_acknowledge(context, entries[j].key, entries[j].revision, NULL));
      }
      uint32_t ready;
      CHECK(rumqttc_notification_prepare_wait(context, &ready, NULL));
      if (!ready) { wait_ready(context, 500); ++idle_waits; }
    } else
#endif
    {
      for (unsigned i = 0; i < count; ++i) drain(clients[i]);
      native_sleep_ms(1); ++idle_waits;
    }
  }
  uint64_t idle_cpu = cpu_ns() - idle_cpu_started;
  puts("PUBLISH"); fflush(stdout);
  rumqttc_publish_options_t options = native_publish_options(RUMQTTC_QOS_1);
  uint64_t begin = now_ns();
  uint64_t publish_cpu_started = cpu_ns();
  unsigned waits = 0;
  for (unsigned round = 0; round < rounds; ++round) {
    for (unsigned i = 0; i < count; ++i) {
      started[i] = now_ns();
      CHECK(rumqttc_client_publish_tracked(clients[i], native_string("readiness/benchmark"),
          native_bytes((const uint8_t *)"0123456789abcdef", 16), &options, &completions[i], NULL));
#if WITH_READINESS
      if (active) CHECK(rumqttc_notification_register_completion(context, completions[i], i, &keys[i], NULL));
#endif
    }
    host_work(work_us);
    unsigned completed = 0;
#if WITH_READINESS
    unsigned service_batches = 0;
#endif
    while (completed != count) {
      REQUIRE(now_ns() - begin < UINT64_C(60000000000));
#if WITH_READINESS
      if (active) {
        rumqttc_notification_ready_t entries[32];
        size_t observed = 0;
        rumqttc_status_t status = rumqttc_notification_collect(context, entries, sizeof(entries[0]), 32, &observed, NULL);
        REQUIRE(status == RUMQTTC_OK || status == RUMQTTC_WOULD_BLOCK);
        for (size_t j = 0; j < observed; ++j) {
          unsigned i = (unsigned)entries[j].tag;
          if (entries[j].source_kind == RUMQTTC_NOTIFICATION_EVENTS) {
            drain_budget(clients[i], reactor ? 64 : SIZE_MAX);
            CHECK(rumqttc_notification_acknowledge(context, entries[j].key, entries[j].revision, NULL));
          } else {
            CHECK(rumqttc_completion_poll(completions[i], NULL));
            latencies[(size_t)round * count + i] = now_ns() - started[i];
            CHECK(rumqttc_notification_deregister(context, entries[j].key, NULL));
            rumqttc_completion_destroy(completions[i]); completions[i] = NULL;
            ++completed;
          }
        }
        if (reactor && completed == count) break;
        if (reactor && observed != 0 && ++service_batches < 4) continue;
        service_batches = 0;
        uint32_t ready;
        CHECK(rumqttc_notification_prepare_wait(context, &ready, NULL));
        if (!ready && completed != count) { wait_ready(context, 5000); ++waits; }
      } else
#endif
      {
        for (unsigned i = 0; i < count; ++i) {
          drain(clients[i]);
          if (completions[i] == NULL) continue;
          rumqttc_status_t status;
          if (blocking) {
            status = rumqttc_completion_wait_timeout_ms(completions[i], 5000, NULL);
            ++waits;
          } else {
            status = rumqttc_completion_poll(completions[i], NULL);
          }
          if (status == RUMQTTC_WOULD_BLOCK) continue;
          CHECK(status);
          latencies[(size_t)round * count + i] = now_ns() - started[i];
          rumqttc_completion_destroy(completions[i]); completions[i] = NULL;
          ++completed;
        }
      }
    }
  }
  uint64_t elapsed = now_ns() - begin;
  uint64_t publish_cpu = cpu_ns() - publish_cpu_started;
  printf("{\"elapsed_ns\":%llu,\"operations\":%llu,\"waits\":%u,\"idle_waits\":%u,"
      "\"idle_cpu_ns\":%" PRIu64 ",\"publish_cpu_ns\":%" PRIu64 ",\"latency_ns\":[",
      (unsigned long long)elapsed, (unsigned long long)((size_t)count * rounds), waits, idle_waits,
      idle_cpu, publish_cpu);
  for (size_t i = 0; i < (size_t)count * rounds; ++i) printf("%s%llu", i == 0 ? "" : ",", (unsigned long long)latencies[i]);
  puts("]}"); fflush(stdout);
#if WITH_READINESS
  if (active) {
    CHECK(rumqttc_notification_context_request_close(context, NULL));
    rumqttc_notification_context_release(context);
  }
  free(keys);
#endif
  for (unsigned i = 0; i < count; ++i) native_close_destroy(clients[i]);
  CHECK(rumqttc_execution_context_request_shutdown(execution, NULL));
  CHECK(rumqttc_execution_context_join_timeout_ms(execution, 5000, NULL));
  rumqttc_execution_context_release(execution);
  free(clients); free(completions); free(started); free(latencies);
  return 0;
}
