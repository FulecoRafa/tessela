#!/usr/bin/env python3
"""stress: publishes an incrementing counter at STRESS_HZ (default 100) for benchmarks."""

import asyncio
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "sdk" / "python"))
from marathon_service import Service  # noqa: E402

HZ = float(os.environ.get("STRESS_HZ", "100"))
svc = Service("stress")
tick = svc.topic("tick", {"n": 0})


async def run():
    loop = asyncio.get_running_loop()
    start, n = loop.time(), 0
    while True:
        n += 1
        tick.publish({"n": n})
        await asyncio.sleep(max(0.0, start + n / HZ - loop.time()))


if __name__ == "__main__":
    svc.run(run())
