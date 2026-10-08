#!/usr/bin/env python3
"""sys: clock, load average and memory, in Python. Reads /proc only."""

import asyncio
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "sdk" / "python"))
from marathon_service import Service  # noqa: E402

WEEKDAYS = ["seg", "ter", "qua", "qui", "sex", "sáb", "dom"]
MONTHS = ["jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez"]


def clock() -> dict:
    t = time.localtime()
    return {
        "hm": time.strftime("%H:%M", t),
        "hms": time.strftime("%H:%M:%S", t),
        "date": f"{WEEKDAYS[t.tm_wday]} {t.tm_mday} {MONTHS[t.tm_mon - 1]}",
    }


def load() -> dict:
    load1 = float(Path("/proc/loadavg").read_text().split()[0])
    cores = os.cpu_count() or 1
    return {"load1": load1, "cores": cores, "ratio": round(min(load1 / cores, 1.0), 3)}


def memory() -> dict:
    info = {}
    for line in Path("/proc/meminfo").read_text().splitlines():
        key, value = line.split(":", 1)
        info[key] = int(value.split()[0])  # kB
    total, avail = info["MemTotal"], info.get("MemAvailable", info["MemFree"])
    used = total - avail
    return {
        "used_gb": round(used / 1024 / 1024, 1),
        "total_gb": round(total / 1024 / 1024, 1),
        "ratio": round(used / total, 3),
    }


svc = Service("sys")
clock_t = svc.topic("clock", clock())
load_t = svc.topic("load", load())
mem_t = svc.topic("memory", memory())


@svc.method("seconds")
def seconds(args):
    """Toggle-free example of a call with a result: current epoch seconds."""
    return int(time.time())


async def tick_clock():
    while True:
        clock_t.publish(clock())
        await asyncio.sleep(1 - (time.time() % 1) + 0.01)


async def tick_system():
    while True:
        load_t.publish(load())
        mem_t.publish(memory())
        await asyncio.sleep(2)


if __name__ == "__main__":
    svc.run(tick_clock(), tick_system())
