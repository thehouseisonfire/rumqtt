from __future__ import annotations

import asyncio
import gc
import os
import sys
import threading
import weakref

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
    task_directory = "/proc/self/task"
    if os.path.isdir(task_directory):
        return len(os.listdir(task_directory))
    return threading.active_count()


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
    warmup = client("python-repetition-warmup")
    await warmup.connect()
    await warmup.close_now()
    del warmup
    gc.collect()
    await asyncio.sleep(0.025)
    baseline = native_thread_count()
    references: list[weakref.ReferenceType[MqttClient]] = []
    for index in range(40):
        mqtt = client(f"python-repetition-{index}")
        references.append(weakref.ref(mqtt))
        await mqtt.connect()
        if index % 3 == 0:
            await mqtt.close_now()
        elif index % 3 == 1:
            await mqtt.close()
        else:
            mqtt._native.abandon()
        del mqtt
    for _ in range(40):
        gc.collect()
        if all(reference() is None for reference in references) and native_thread_count() <= baseline:
            break
        await asyncio.sleep(0.025)
    assert all(reference() is None for reference in references)
    assert native_thread_count() <= baseline


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
