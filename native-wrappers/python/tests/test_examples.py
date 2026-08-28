from __future__ import annotations

import asyncio
import runpy
from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import cast

import pytest

WaitForCleanup = Callable[[asyncio.Task[None]], Awaitable[asyncio.CancelledError | None]]
wait_for_cleanup = cast(
    WaitForCleanup,
    runpy.run_path(str(Path(__file__).parents[1] / "examples" / "application.py"))["wait_for_cleanup"],
)


@pytest.mark.asyncio
async def test_cleanup_finishes_before_repeated_cancellation_is_propagated() -> None:
    release = asyncio.Event()
    finished = asyncio.Event()

    async def cleanup() -> None:
        await release.wait()
        finished.set()

    async def owner() -> None:
        cleanup_task = asyncio.create_task(cleanup())
        cancellation = await wait_for_cleanup(cleanup_task)
        assert finished.is_set()
        assert cancellation is not None
        raise cancellation

    owner_task = asyncio.create_task(owner())
    await asyncio.sleep(0)
    owner_task.cancel()
    await asyncio.sleep(0)
    assert not owner_task.done()
    owner_task.cancel()
    await asyncio.sleep(0)
    assert not owner_task.done()

    release.set()
    with pytest.raises(asyncio.CancelledError):
        await owner_task
    assert finished.is_set()
