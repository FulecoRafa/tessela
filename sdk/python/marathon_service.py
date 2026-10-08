"""Python helper for writing Marathon services. Standard library only.

    svc = Service("sys")
    load = svc.topic("load", {"load1": 0.0})

    @svc.method("ping")
    def ping(args):
        return "pong"

    async def tick():
        while True:
            load.publish(read_load())
            await asyncio.sleep(2)

    svc.run(tick())

Protocol: one JSON object per line over a Unix socket; ops are get, call,
subscribe, unsubscribe and describe. Subscriptions receive the current value
first, then every change.
"""

from __future__ import annotations

import asyncio
import json
import os
import signal
import sys
from pathlib import Path
from typing import Any, Awaitable, Callable


def socket_path(name: str) -> Path:
    base = os.environ.get("MARATHON_SERVICES_DIR")
    if not base:
        runtime = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
        base = os.path.join(runtime, "marathon", "services")
    return Path(base) / f"{name}.sock"


class _Client:
    def __init__(self, writer: asyncio.StreamWriter):
        self.writer = writer

    def send(self, msg: dict) -> None:
        self.writer.write((json.dumps(msg, separators=(",", ":")) + "\n").encode())


class Topic:
    def __init__(self, service: "Service", path: str, value: Any):
        self._service = service
        self.path = path
        self.value = value
        self._subs: dict[_Client, set[int]] = {}

    def publish(self, value: Any) -> None:
        if value == self.value:
            return
        self.value = value
        for client, ids in list(self._subs.items()):
            for sub_id in ids:
                client.send({"id": sub_id, "event": value})


class Service:
    def __init__(self, name: str):
        self.name = name
        self.topics: dict[str, Topic] = {}
        self.methods: dict[str, Callable[[Any], Any]] = {}

    def topic(self, path: str, initial: Any) -> Topic:
        t = Topic(self, path, initial)
        self.topics[path] = t
        return t

    def method(self, name: str):
        def register(fn: Callable[[Any], Any]):
            self.methods[name] = fn
            return fn
        return register

    async def _handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        client = _Client(writer)
        try:
            while line := await reader.readline():
                try:
                    req = json.loads(line)
                except json.JSONDecodeError as e:
                    client.send({"id": 0, "error": f"bad request: {e}"})
                    continue
                await self._dispatch(client, req)
                await writer.drain()
        except ConnectionError:
            pass
        finally:
            for t in self.topics.values():
                t._subs.pop(client, None)
            writer.close()

    async def _dispatch(self, client: _Client, req: dict) -> None:
        rid, op = req.get("id", 0), req.get("op")
        if op in ("get", "subscribe"):
            t = self.topics.get(req.get("path", ""))
            if t is None:
                client.send({"id": rid, "error": f"unknown path {req.get('path')}"})
            elif op == "get":
                client.send({"id": rid, "result": t.value})
            else:
                t._subs.setdefault(client, set()).add(rid)
                client.send({"id": rid, "event": t.value})
        elif op == "unsubscribe":
            for t in self.topics.values():
                t._subs.get(client, set()).discard(req.get("sub"))
            client.send({"id": rid, "result": None})
        elif op == "call":
            fn = self.methods.get(req.get("method", ""))
            if fn is None:
                client.send({"id": rid, "error": f"unknown method {req.get('method')}"})
                return
            try:
                result = fn(req.get("args"))
                if asyncio.iscoroutine(result):
                    result = await result
                client.send({"id": rid, "result": result})
            except Exception as e:  # report to the caller, keep serving
                client.send({"id": rid, "error": str(e)})
        elif op == "describe":
            client.send({"id": rid, "result": {
                "name": self.name,
                "paths": sorted(self.topics),
                "methods": sorted(self.methods),
            }})
        else:
            client.send({"id": rid, "error": f"unknown op {op}"})

    async def serve(self, *background: Awaitable[Any]) -> None:
        path = socket_path(self.name)
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        path.unlink(missing_ok=True)
        server = await asyncio.start_unix_server(self._handle, path=str(path))
        os.chmod(path, 0o600)
        print(f"{self.name}: listening on {path}", file=sys.stderr)
        stop = asyncio.Event()
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, stop.set)
        tasks = [asyncio.ensure_future(b) for b in background]
        async with server:
            await stop.wait()
        for t in tasks:
            t.cancel()
        path.unlink(missing_ok=True)

    def run(self, *background: Awaitable[Any]) -> None:
        asyncio.run(self.serve(*background))
