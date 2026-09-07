from __future__ import annotations

import asyncio
import gc
import os
import sys
import weakref

import psutil
from rumqttc import MqttClient, MqttClientOptions, ProtocolVersion


def client(client_id: str) -> MqttClient:
    return MqttClient(
        MqttClientOptions(
            protocol=ProtocolVersion.MQTT_5_0,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id=client_id,
        )
    )


def native_thread_count() -> int:
    # threading.active_count() sees only Python-managed threads. Process.num_threads() uses the
    # platform process API and therefore includes the Rust MQTT driver and Tokio worker threads.
    return psutil.Process().num_threads()


async def gc_cycle() -> None:
    mqtt = client("python-gc-cycle")
    await mqtt.connect()
    reference = weakref.ref(mqtt)
    cycle: list[object] = []
    cycle.extend((cycle, mqtt))
    del mqtt, cycle
    for _ in range(20):
        gc.collect()
        if reference() is None:
            break
        await asyncio.sleep(0.01)
    assert reference() is None


async def repetition() -> None:
    # Exercise every shutdown path before measuring. Tokio's blocking pool and platform network
    # support may create a persistent helper thread the first time a particular path is used.
    # The measured loop mixes graceful close and immediate close, so both must be warmed up;
    # otherwise the first close_now() can retain a helper thread beyond the baseline on
    # macOS/Windows runners.
    for lifecycle in ("close", "close_now", "abandon"):
        warmup = client(f"python-repetition-warmup-{lifecycle}")
        await warmup.connect()
        if lifecycle == "close":
            await warmup.close()
        elif lifecycle == "close_now":
            await warmup.close_now()
        else:
            warmup._native.abandon()
        reference = weakref.ref(warmup)
        del warmup
        for _ in range(200):
            gc.collect()
            if reference() is None:
                break
            await asyncio.sleep(0.025)
        assert reference() is None, f"{lifecycle} warmup"
    # Slow and instrumented CI runners may need extra time for the warmup drivers to fully
    # exit before the baseline is taken. Stabilize briefly so the baseline does not miss a
    # lingering warmup thread and then flake as an apparent leak.
    for _ in range(20):
        gc.collect()
        await asyncio.sleep(0.025)
    await asyncio.sleep(0.1)
    baseline = native_thread_count()
    for lifecycle in ("close", "abandon"):
        references: list[weakref.ReferenceType[MqttClient]] = []
        for index in range(30):
            mqtt = client(f"python-repetition-{lifecycle}-{index}")
            references.append(weakref.ref(mqtt))
            await mqtt.connect()
            if lifecycle == "close":
                await (mqtt.close() if index % 2 else mqtt.close_now())
            else:
                mqtt._native.abandon()
            del mqtt
        # Abandonment is deliberately nonblocking. Slow and instrumented CI runners may need
        # several seconds to schedule all signaled native drivers through runtime teardown.
        # Windows and macOS runners have been observed needing beyond 5s under load, so allow
        # up to ~10s while remaining a bounded leak check.
        for _ in range(400):
            gc.collect()
            if all(reference() is None for reference in references) and native_thread_count() <= baseline:
                break
            await asyncio.sleep(0.025)
        assert all(reference() is None for reference in references), lifecycle
        assert native_thread_count() <= baseline, lifecycle


async def module_teardown() -> None:
    mqtt = client("python-module-teardown")
    await mqtt.connect()
    for name in tuple(sys.modules):
        if name == "rumqttc" or name.startswith("rumqttc."):
            sys.modules.pop(name, None)
    del mqtt
    gc.collect()


async def explicit_exit() -> None:
    mqtt = client("python-explicit-sys-exit")
    await mqtt.connect()
    sys.exit(0)


async def abrupt_exit() -> None:
    mqtt = client("python-abrupt-exit")
    await mqtt.connect()
    os._exit(0)


async def main() -> None:
    mode = sys.argv[1]
    operation = {
        "gc-cycle": gc_cycle,
        "repetition": repetition,
        "module-teardown": module_teardown,
        "explicit-exit": explicit_exit,
        "abrupt-exit": abrupt_exit,
    }[mode]
    await operation()


asyncio.run(main())
