"""Tokio completion-path and resource benchmark retained from the bridge experiment.

Build with ``maturin develop --release --features benchmark-testing``, then run this script in a
fresh broker-fixture process with ``RUMQTTC_TOKIO_BLOCKING_THREADS`` set to the cap under test.
Normal wheels neither expose the probe nor read the cap override.
"""

from __future__ import annotations

import asyncio
import gc
import json
import os
import platform
import resource
import statistics
import sys
import time
import weakref
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import Any, TypeVar

from rumqttc import (
    Closed,
    ErrorKind,
    IncomingPublish,
    MqttClient,
    MqttClientOptions,
    MqttError,
    ProtocolVersion,
    PublishOptions,
    QoS,
    Subscription,
    _native,
)

T = TypeVar("T")
HOST = os.environ["RUMQTTC_TEST_HOST"]
PORT = int(os.environ["RUMQTTC_TEST_PORT"])
SAMPLES = int(os.environ.get("RUMQTTC_BENCHMARK_SAMPLES", "600"))
BLOCKING_THREADS = int(_native._TOKIO_BLOCKING_THREADS)  # type: ignore[attr-defined]
RUN_LABEL = f"tokio-cap-{BLOCKING_THREADS}"


def options(client_id: str, **changes: Any) -> MqttClientOptions:
    values: dict[str, Any] = {
        "protocol": ProtocolVersion.MQTT_5_0,
        "broker_host": HOST,
        "broker_port": PORT,
        "client_id": client_id,
        "request_capacity": 256,
        "event_capacity": 1024,
    }
    values.update(changes)
    return MqttClientOptions(**values)


def distribution(values: list[float]) -> dict[str, float | int]:
    ordered = sorted(values)

    def percentile(value: float) -> float:
        return ordered[min(len(ordered) - 1, int((len(ordered) - 1) * value))]

    return {
        "samples": len(values),
        "median_us": statistics.median(values) / 1_000,
        "p95_us": percentile(0.95) / 1_000,
        "p99_us": percentile(0.99) / 1_000,
    }


async def timed_samples(operation: Callable[[], Awaitable[T]], count: int) -> dict[str, float | int]:
    values: list[float] = []
    for _ in range(count):
        started = time.perf_counter_ns()
        await operation()
        values.append(float(time.perf_counter_ns() - started))
    return distribution(values)


def rss_bytes() -> int:
    status = Path("/proc/self/status")
    if status.exists():
        for line in status.read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) * 1024
    scale = 1 if sys.platform == "darwin" else 1024
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * scale


def thread_count() -> int:
    tasks = Path("/proc/self/task")
    return len(tuple(tasks.iterdir())) if tasks.exists() else len(__import__("threading").enumerate())


def cpu_model() -> str:
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    return platform.processor()


async def completion_probe(client: MqttClient) -> dict[str, float | int]:
    for _ in range(100):
        await client._native._completion_probe("ok")  # type: ignore[attr-defined]
    return await timed_samples(lambda: client._native._completion_probe("ok"), SAMPLES)  # type: ignore[attr-defined]


async def completion_burst(client: MqttClient, count: int) -> dict[str, Any]:
    elapsed: list[float] = []
    runs = 8
    for _ in range(runs):
        started = time.perf_counter_ns()
        values = await asyncio.gather(
            *(client._native._completion_probe(str(index)) for index in range(count))  # type: ignore[attr-defined]
        )
        assert len(values) == count
        elapsed.append(float(time.perf_counter_ns() - started))
    return {
        **distribution(elapsed),
        "operations_per_run": count,
        "runs": runs,
        "median_operations_per_s": count / (statistics.median(elapsed) / 1_000_000_000),
    }


async def throughput(client: MqttClient, concurrency: int, count: int) -> dict[str, Any]:
    rates: list[float] = []
    elapsed_values: list[float] = []
    for _ in range(12):
        queue: asyncio.Queue[int] = asyncio.Queue()
        for sequence in range(count):
            queue.put_nowait(sequence)

        async def worker(queue: asyncio.Queue[int] = queue) -> None:
            while not queue.empty():
                try:
                    sequence = queue.get_nowait()
                except asyncio.QueueEmpty:
                    return
                await client.publish("rumqttc/cap/throughput", sequence.to_bytes(8, "big"))

        started = time.perf_counter()
        await asyncio.gather(*(worker() for _ in range(concurrency)))
        elapsed = time.perf_counter() - started
        elapsed_values.append(elapsed)
        rates.append(count / elapsed)
    ordered = sorted(rates)
    return {
        "operations_per_run": count,
        "runs": len(rates),
        "median_operations_per_s": statistics.median(rates),
        "p05_operations_per_s": ordered[0],
        "p95_operations_per_s": ordered[int((len(ordered) - 1) * 0.95)],
        "elapsed_s": elapsed_values,
        "operations_per_s": rates,
    }


async def event_path(client: MqttClient, count: int) -> dict[str, float | int]:
    topic = f"rumqttc/cap/events/{os.getpid()}"
    events = client.events()
    await anext(events)
    await client.subscribe([Subscription(topic, QoS.AT_MOST_ONCE)])
    starts: dict[int, int] = {}
    latencies: list[float] = []

    async def consume() -> None:
        while len(latencies) < count:
            event = await anext(events)
            if isinstance(event, IncomingPublish) and event.topic == topic:
                sequence = int.from_bytes(event.payload, "big")
                latencies.append(float(time.perf_counter_ns() - starts.pop(sequence)))

    consumer = asyncio.create_task(consume())
    began = time.perf_counter()
    for sequence in range(count):
        starts[sequence] = time.perf_counter_ns()
        await client.publish(topic, sequence.to_bytes(8, "big"))
    await consumer
    elapsed = time.perf_counter() - began
    return {
        **distribution(latencies),
        "events_per_s": count / elapsed,
    }


async def connect_close(count: int) -> dict[str, Any]:
    connect: list[float] = []
    close: list[float] = []
    for sequence in range(count):
        client = MqttClient(options(f"cap-churn-{BLOCKING_THREADS}-{sequence}"))
        started = time.perf_counter_ns()
        await client.connect()
        connect.append(float(time.perf_counter_ns() - started))
        started = time.perf_counter_ns()
        await client.close()
        close.append(float(time.perf_counter_ns() - started))
    return {"connect": distribution(connect), "close": distribution(close)}


async def resource_scaling(counts: tuple[int, ...]) -> list[dict[str, float | int]]:
    output: list[dict[str, float | int]] = []
    for count in counts:
        clients = [MqttClient(options(f"cap-resource-{BLOCKING_THREADS}-{count}-{index}")) for index in range(count)]
        connect_started = time.perf_counter_ns()
        await asyncio.gather(*(client.connect() for client in clients))
        connect_elapsed = time.perf_counter_ns() - connect_started
        cpu_started = time.process_time()
        wall_started = time.perf_counter()
        await asyncio.sleep(0.5)
        cpu = time.process_time() - cpu_started
        wall = time.perf_counter() - wall_started
        entry: dict[str, float | int] = {
            "clients": count,
            "connect_elapsed_us": connect_elapsed / 1_000,
            "rss_bytes": rss_bytes(),
            "native_threads": thread_count(),
            "idle_cpu_percent": 100 * cpu / wall,
        }
        close_started = time.perf_counter_ns()
        await asyncio.gather(*(client.close_now() for client in clients))
        entry["close_elapsed_us"] = (time.perf_counter_ns() - close_started) / 1_000
        output.append(entry)
    return output


async def callback_backlog(client: MqttClient) -> dict[str, float | int]:
    futures = [client._native._completion_probe(str(index)) for index in range(2048)]  # type: ignore[attr-defined]
    rss_before = rss_bytes()
    threads_before = thread_count()
    time.sleep(0.2)
    threads_stalled = thread_count()
    done_while_stalled = sum(future.done() for future in futures)
    resumed = time.perf_counter_ns()
    values = await asyncio.gather(*futures)
    return {
        "scheduled": len(futures),
        "done_while_loop_stalled": done_while_stalled,
        "rss_growth_bytes": rss_bytes() - rss_before,
        "threads_before": threads_before,
        "threads_stalled": threads_stalled,
        "thread_growth": threads_stalled - threads_before,
        "drain_us": (time.perf_counter_ns() - resumed) / 1_000,
        "delivered": len(values),
    }


async def retained_objects(count: int) -> dict[str, int]:
    references: list[weakref.ReferenceType[MqttClient]] = []
    for sequence in range(count):
        client = MqttClient(options(f"cap-retained-{BLOCKING_THREADS}-{sequence}"))
        await client.connect()
        references.append(weakref.ref(client))
        await client.close_now()
        del client
    for _ in range(5):
        gc.collect()
        await asyncio.sleep(0)
    return {"created": count, "retained_clients": sum(reference() is not None for reference in references)}


async def pending_shutdown(count: int) -> dict[str, float | int]:
    values: list[float] = []
    for sequence in range(count):
        client = MqttClient(options(f"cap-pending-{BLOCKING_THREADS}-{sequence}", broker_port=1, connection_timeout=1))
        connecting = asyncio.create_task(client.connect())
        await asyncio.sleep(0)
        started = time.perf_counter_ns()
        await client.close_now()
        values.append(float(time.perf_counter_ns() - started))
        await asyncio.gather(connecting, return_exceptions=True)
    return distribution(values)


async def saturated_close_deadline(client: MqttClient) -> dict[str, float | int | str]:
    closing = MqttClient(options(f"cap-deadline-{BLOCKING_THREADS}"))
    await closing.connect()
    events = closing.events()
    await anext(events)
    blockers = [
        asyncio.ensure_future(client._native._blocking_probe(250))  # type: ignore[attr-defined]
        for _ in range(BLOCKING_THREADS - 1)
    ]
    await asyncio.sleep(0.02)
    started = time.perf_counter_ns()
    try:
        await closing.close(timeout=0.05)
    except MqttError as error:
        elapsed_us = (time.perf_counter_ns() - started) / 1_000
        assert error.kind is ErrorKind.TIMEOUT
        error_kind = error.kind.value
    else:
        raise AssertionError("saturated close unexpectedly completed within its timeout")

    async def terminal_event() -> Closed:
        async for event in events:
            if isinstance(event, Closed):
                return event
        raise AssertionError("graceful-close escalation ended without a terminal event")

    terminal_started = time.perf_counter_ns()
    terminal = await asyncio.wait_for(terminal_event(), timeout=0.15)
    terminal_us = (time.perf_counter_ns() - terminal_started) / 1_000
    assert not terminal.graceful
    await asyncio.gather(*blockers)
    return {
        "budget_us": 50_000,
        "elapsed_us": elapsed_us,
        "error_kind": error_kind,
        "terminal_after_timeout_us": terminal_us,
    }


async def saturated_immediate_dispatch(client: MqttClient) -> dict[str, float | int | str]:
    closing = MqttClient(options(f"cap-immediate-{BLOCKING_THREADS}"))
    await closing.connect()
    events = closing.events()
    await anext(events)
    blockers = [
        asyncio.ensure_future(client._native._blocking_probe(250))  # type: ignore[attr-defined]
        for _ in range(BLOCKING_THREADS - 1)
    ]
    await asyncio.sleep(0.02)
    started = time.perf_counter_ns()
    response = json.loads(await closing._native.close_now(50))
    elapsed_us = (time.perf_counter_ns() - started) / 1_000
    assert not response["ok"]
    assert response["error"]["kind"] == ErrorKind.TIMEOUT.value

    async def terminal_event() -> Closed:
        async for event in events:
            if isinstance(event, Closed):
                return event
        raise AssertionError("immediate close ended without a terminal event")

    terminal_started = time.perf_counter_ns()
    terminal = await asyncio.wait_for(terminal_event(), timeout=0.15)
    terminal_us = (time.perf_counter_ns() - terminal_started) / 1_000
    assert not terminal.graceful
    await asyncio.gather(*blockers)
    return {
        "budget_us": 50_000,
        "elapsed_us": elapsed_us,
        "error_kind": response["error"]["kind"],
        "terminal_after_timeout_us": terminal_us,
    }


async def saturated_graceful_cancellation(client: MqttClient) -> dict[str, float | int | str]:
    closing = MqttClient(options(f"cap-cancel-{BLOCKING_THREADS}"))
    await closing.connect()
    events = closing.events()
    await anext(events)
    blockers = [
        asyncio.ensure_future(client._native._blocking_probe(250))  # type: ignore[attr-defined]
        for _ in range(BLOCKING_THREADS - 1)
    ]
    await asyncio.sleep(0.02)
    close = asyncio.create_task(closing.close(timeout=1.0))
    await asyncio.sleep(0.02)
    canceled_at = time.perf_counter_ns()
    close.cancel()
    try:
        await close
    except asyncio.CancelledError:
        pass
    else:
        raise AssertionError("saturated graceful close was not canceled")

    async def terminal_event() -> Closed:
        async for event in events:
            if isinstance(event, Closed):
                return event
        raise AssertionError("canceled graceful close ended without a terminal event")

    terminal = await asyncio.wait_for(terminal_event(), timeout=0.15)
    terminal_us = (time.perf_counter_ns() - canceled_at) / 1_000
    assert not terminal.graceful
    await asyncio.gather(*blockers)
    return {"cancel_to_terminal_us": terminal_us, "terminal": "immediate"}


async def main() -> None:
    initial_threads = thread_count()
    client = MqttClient(options(f"cap-main-{BLOCKING_THREADS}"))
    probe = await completion_probe(client)
    bursts = {str(count): await completion_burst(client, count) for count in (32, 256, 2048)}
    await client.connect()
    for _ in range(50):
        await client.publish("rumqttc/cap/warmup", b"warmup")
    metrics: dict[str, Any] = {
        "deterministic_completion": probe,
        "completion_bursts": bursts,
        "admission_qos0": await timed_samples(
            lambda: client.enqueue_publish(
                "rumqttc/cap/admission",
                b"x" * 64,
                PublishOptions(qos=QoS.AT_MOST_ONCE),
            ),
            SAMPLES,
        ),
        "tracked_qos0": await timed_samples(
            lambda: client.publish(
                "rumqttc/cap/qos0",
                b"x" * 64,
                PublishOptions(qos=QoS.AT_MOST_ONCE),
            ),
            SAMPLES,
        ),
        "tracked_qos1": await timed_samples(
            lambda: client.publish(
                "rumqttc/cap/qos1",
                b"x" * 64,
                PublishOptions(qos=QoS.AT_LEAST_ONCE),
            ),
            SAMPLES,
        ),
    }
    metrics["throughput"] = {
        str(concurrency): await throughput(client, concurrency, 600) for concurrency in (1, 32, 256)
    }
    metrics["events"] = await event_path(client, min(SAMPLES, 400))
    metrics["callback_backlog"] = await callback_backlog(client)
    metrics["saturated_close_deadline"] = await saturated_close_deadline(client)
    metrics["saturated_immediate_dispatch"] = await saturated_immediate_dispatch(client)
    metrics["saturated_graceful_cancellation"] = await saturated_graceful_cancellation(client)
    await client.close()
    metrics["connect_close"] = await connect_close(20)
    metrics["resource_scaling"] = await resource_scaling((1, 10, 100))
    metrics["retained_objects"] = await retained_objects(50)
    metrics["pending_shutdown"] = await pending_shutdown(20)

    print(
        json.dumps(
            {
                "schema_version": 2,
                "backend": "tokio",
                "variant": RUN_LABEL,
                "blocking_threads": BLOCKING_THREADS,
                "environment": {
                    "platform": platform.platform(),
                    "python": sys.version,
                    "machine": platform.machine(),
                    "processor": cpu_model(),
                    "rust_profile": "release",
                    "broker": "mosquitto 2.1.2",
                    "tls": False,
                    "initial_threads": initial_threads,
                },
                "metrics": metrics,
            },
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    asyncio.run(main())
