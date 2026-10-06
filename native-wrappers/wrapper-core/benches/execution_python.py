"""Fresh-process asyncio wrapper consumer; requires a benchmark-testing extension."""

import asyncio
import gc
import json
import os
import sys
import time
from pathlib import Path

from rumqttc import (
    MqttClient,
    MqttClientOptions,
    ProtocolVersion,
    PublishOptions,
    QoS,
    Subscription,
    TcpTransport,
    TlsOptions,
    TlsTransport,
    WebSocketTransport,
    WssTransport,
    _native,
)


async def until_event(client, wanted):
    while True:
        response = json.loads(await client._native.next_event())
        event = response.get("event", {})
        if not response.get("ok") or response.get("done") or event.get("type") in ["driverError", "closed"]:
            raise RuntimeError(f"Event stream ended before {wanted}: {response}")
        if event.get("type") == wanted:
            return


async def main():
    mode, count, protocol, qos, scenario = sys.argv[1:]
    count, protocol, qos = int(count), int(protocol), int(qos)
    rounds = int(os.environ.get("RUMQTTC_EXECUTION_ROUNDS", "10"))
    contexts = [
        _native._BenchmarkExecutionContext(count)
        for _ in range(0 if mode == "dedicated" else 2 if mode == "shards" else 1)
    ]
    transport_name = os.environ.get("RUMQTTC_EXECUTION_TRANSPORT", "tcp")
    port = int(os.environ["RUMQTTC_TEST_PORT"])
    tls = (
        TlsOptions(ca=Path(os.environ["RUMQTTC_EXECUTION_CA"]).read_bytes())
        if transport_name in ["tls", "wss"]
        else None
    )
    transport = {
        "tcp": lambda: TcpTransport(),
        "tls": lambda: TlsTransport(tls),
        "ws": lambda: WebSocketTransport(f"ws://localhost:{port}/mqtt"),
        "wss": lambda: WssTransport(f"wss://localhost:{port}/mqtt", tls),
    }[transport_name]()
    clients = []
    started = time.perf_counter_ns()
    for index in range(count):
        client = MqttClient(
            MqttClientOptions(
                protocol=ProtocolVersion.MQTT_3_1_1 if protocol == 1 else ProtocolVersion.MQTT_5_0,
                broker_host="localhost" if transport_name in ["tls", "wss"] else "127.0.0.1",
                broker_port=port,
                transport=transport,
                client_id=f"execution-python-{index}",
                event_capacity=1024,
            )
        )
        if contexts:
            client._native._benchmark_set_execution(contexts[index % len(contexts)])
        try:
            await client.connect()
        except Exception as error:
            print(
                json.dumps({"phase": "failed_start", "started_clients": len(clients), "error": str(error)}), flush=True
            )
            await asyncio.gather(*(owner.close_now() for owner in [*clients, client]), return_exceptions=True)
            for context in contexts:
                context.shutdown()
                await asyncio.to_thread(context.join, 30000)
            return
        clients.append(client)
        # connect() observes a connection watch; the initial event remains in the event queue.
        await until_event(client, "connected")
        if scenario == "incoming":
            await client.subscribe([Subscription("execution/traffic", QoS.AT_MOST_ONCE)])
    print(json.dumps({"phase": "ready", "startup_ns": time.perf_counter_ns() - started}), flush=True)
    await asyncio.to_thread(sys.stdin.readline)
    admission, completion, events, loop_delay = [], [], [], []

    async def loop_probe():
        while True:
            before = time.perf_counter_ns()
            await asyncio.sleep(0.005)
            loop_delay.append(max(0, time.perf_counter_ns() - before - 5_000_000))

    probe = asyncio.create_task(loop_probe())
    busy_admitted = 0
    stop_busy = False

    async def busy_publish():
        nonlocal busy_admitted
        while not stop_busy:
            await clients[0].publish("execution/busy", b"x" * 64, PublishOptions(qos=QoS.AT_MOST_ONCE))
            busy_admitted += 1

    producers = [asyncio.create_task(busy_publish()) for _ in range(8)] if scenario == "hotspot" else []
    started = time.perf_counter_ns()
    if scenario == "reconnect":
        # Use each client's native event stream to await the next connection generation.
        for client in clients:
            await until_event(client, "connected")
    elif scenario == "idle":
        await asyncio.sleep(0.2)
    else:

        async def publish(client):
            begin = time.perf_counter_ns()
            await client.publish("execution/traffic", b"x" * 64, PublishOptions(qos=QoS(qos)))
            completion.append(time.perf_counter_ns() - begin)

        for _ in range(rounds):
            if scenario == "periodic":
                await asyncio.sleep(0.1)
            begin = time.perf_counter_ns()
            await asyncio.gather(*(publish(client) for client in (clients[:1] if scenario == "incoming" else clients)))
            if scenario == "incoming":
                for client in clients:
                    await until_event(client, "publish")
                    events.append(time.perf_counter_ns() - begin)
    elapsed = time.perf_counter_ns() - started
    stop_busy = True
    await asyncio.gather(*producers)
    probe.cancel()
    await asyncio.gather(probe, return_exceptions=True)
    teardown = time.perf_counter_ns()
    for context in contexts:
        context.shutdown()
    await asyncio.gather(*(client.close_now() for client in clients))
    clients.clear()
    del client
    gc.collect()
    for context in contexts:
        await asyncio.to_thread(context.join, 30000)
    print(
        json.dumps(
            {
                "phase": "finished",
                "elapsed_ns": elapsed,
                "busy_admitted": busy_admitted,
                "teardown_ns": time.perf_counter_ns() - teardown,
                "admission_ns": admission,
                "completion_ns": completion,
                "event_ns": events,
                "loop_delay_ns": loop_delay,
            }
        ),
        flush=True,
    )
    await asyncio.to_thread(sys.stdin.readline)


asyncio.run(main())
