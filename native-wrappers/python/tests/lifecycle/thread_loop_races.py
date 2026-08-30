from __future__ import annotations

import asyncio
import concurrent.futures
import os
import threading
from collections.abc import Callable, Coroutine
from typing import Any

from rumqttc import MqttClient, MqttClientOptions, ProtocolVersion, PublishOptions, QoS


def run_protocol(protocol: ProtocolVersion) -> None:
    loop = asyncio.new_event_loop()
    accepting = threading.Event()
    accepting.set()
    started = threading.Event()
    closed = threading.Event()
    diagnostics: list[dict[str, object]] = []
    owner_errors: list[BaseException] = []
    client = MqttClient(
        MqttClientOptions(
            protocol=protocol,
            broker_host=os.environ["RUMQTTC_TEST_HOST"],
            broker_port=int(os.environ["RUMQTTC_TEST_PORT"]),
            client_id=f"python-thread-loop-race-{protocol.value}",
            request_capacity=2,
        )
    )

    def exception_handler(_loop: asyncio.AbstractEventLoop, context: dict[str, object]) -> None:
        diagnostics.append(context)

    async def initialize() -> None:
        await client.connect()
        started.set()

    async def finish() -> None:
        tasks = [task for task in asyncio.all_tasks(loop) if task is not asyncio.current_task(loop)]
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        await client.close_now()

    def own_loop() -> None:
        asyncio.set_event_loop(loop)
        loop.set_exception_handler(exception_handler)
        try:
            initializing = loop.create_task(initialize())
            loop.run_forever()
            _ = initializing
            loop.run_until_complete(finish())
        except BaseException as error:
            owner_errors.append(error)
        finally:
            loop.close()
            closed.set()

    owner = threading.Thread(target=own_loop, name=f"python-loop-owner-{protocol.value}")
    owner.start()
    assert started.wait(timeout=3)

    def schedule(factory: Callable[[], Coroutine[Any, Any, object]]) -> concurrent.futures.Future[object]:
        if not accepting.is_set():
            raise RuntimeError("owner event loop is shutting down")
        coroutine = factory()
        try:
            return asyncio.run_coroutine_threadsafe(coroutine, loop)
        except BaseException:
            coroutine.close()
            raise

    outcomes: set[str] = set()
    futures: list[concurrent.futures.Future[object]] = []
    for index in range(40):
        topic = "rumqttc/native/pressure" if index % 2 else f"thread/race/{index}"

        async def publish(topic: str = topic, index: int = index) -> object:
            assert asyncio.get_running_loop() is loop
            return await client.publish(
                topic,
                index.to_bytes(2, "big"),
                PublishOptions(qos=QoS.AT_LEAST_ONCE),
            )

        future = schedule(publish)
        futures.append(future)
        if index % 3 == 0:
            future.cancel()

    # Successful MQTT completions and caller cancellation are both exercised before shutdown.
    for future in futures[:10]:
        try:
            future.result(timeout=2)
        except concurrent.futures.CancelledError:
            outcomes.add("cancelled")
        else:
            outcomes.add("completed")
    accepting.clear()
    try:
        schedule(lambda: client.publish("thread/rejected", b"rejected"))
    except RuntimeError:
        outcomes.add("rejected")
    else:
        raise AssertionError("new cross-thread scheduling was accepted during loop shutdown")

    loop.call_soon_threadsafe(loop.stop)
    assert closed.wait(timeout=5)
    owner.join(timeout=1)
    assert not owner.is_alive()
    for future in futures:
        try:
            future.result(timeout=1)
        except concurrent.futures.CancelledError:
            outcomes.add("cancelled")
        except Exception:
            outcomes.add("closed")
        else:
            outcomes.add("completed")
    assert {"completed", "cancelled", "rejected"}.issubset(outcomes)
    assert outcomes.issubset({"completed", "cancelled", "closed", "rejected"})
    assert not owner_errors
    assert not diagnostics


for version in (ProtocolVersion.MQTT_3_1_1, ProtocolVersion.MQTT_5_0):
    run_protocol(version)
