# tessela-service

Write [tessela](../../README.md) (and Marathon) services in Rust.

A service is a Unix socket `<dir>/<name>.sock` speaking one JSON object per
line: widgets `subscribe` to **topics** (they get the current value, then
every change) and `call` **methods**. This crate speaks the protocol, keeps
any number of clients and cleans the socket up; it is the Rust sibling of
`sdk/python/marathon_service.py` and `sdk/go/marathon`, with the same wire
behavior (error texts included).

```rust
use std::time::Duration;
use serde::Deserialize;
use tessela_service::{Service, Timer};

#[derive(Deserialize)]
struct Reset { #[serde(default)] to: u64 }

fn main() -> std::io::Result<()> {
    let mut svc = Service::new("counter");
    let count = svc.topic("count", 0u64);           // any Serialize + Clone
    let c = count.clone();
    svc.method_sync("reset", move |a: Reset| { c.publish(a.to); Ok(a.to) });
    svc.run(async move {
        loop {
            Timer::after(Duration::from_secs(1)).await;
            count.update(|n| *n += 1);              // equal values are not re-sent
        }
    })
}
```

## Several services in one process

```rust
use tessela_service::{Host, Service};

let audio = Service::new("audio");
let power = Service::new("power");
Host::new().add(audio).add(power).run(async { /* spawn the data sources */ })?;
```

One socket per service, one thread for all of them. A project that ships
several plugins can build them as one binary and declare it once in
`tessela.toml`:

```toml
[plugins.services]
command = ["services/target/release/my-services"]
services = ["audio", "power"]   # sockets it serves: the runner waits for them
```

## API

| | |
|---|---|
| `Service::new(name)` | a service; `topic(path, initial)` → `Topic<T>`; `method(name, async fn)`; `method_sync(name, fn)`; `describe()`; `run(background)` |
| `Topic<T>` | `publish(v)` (skipped when the JSON is equal), `update(\|v\| ..)`, `get()`; `Send + Sync`, publish from anywhere |
| methods | `Fn(A) -> Result<R>` (or a future of it): `A: Deserialize` (`Value` for raw args; no args reads as `{}`), `R: Serialize` (`()` is `null`); `Err(e)` answers `{"error": "<e>"}`. Calls run concurrently |
| `Host` | `add(service)`, `dir(path)`, `shutdown_handle()`, `run(main)` |
| `spawn(fut)` | another task on the host's thread (may hold `Rc`/`RefCell`) |
| `unblock(f)` | blocking work (files, HTTP) on a small thread pool |
| `Timer`, `Wake`, `poll(interval, &wake, read)` | waiting, waking early, the classic poll loop |
| `process::{run, run_status, follow, command}` | commands with timeouts; `follow` restarts a long-lived one and hands over its lines; children die with the service (feature `process`, default) |
| `dbus::{session, system, watch, signals_from}` | one shared zbus connection per bus; match-rule streams; "the owner of this name signalled something" (feature `dbus`) |

**Socket path:** `$TESSELA_SERVICES_DIR`, else `$MARATHON_SERVICES_DIR` (the
runner sets both), else `$XDG_RUNTIME_DIR/marathon/services`. A leftover
socket nobody answers on is replaced; a live one makes `run` fail instead of
stealing it. Sockets are `0600`.

**Shutdown:** SIGTERM, SIGINT, the parent dying (`PR_SET_PDEATHSIG`; opt out
with `keep_running_without_parent`) or `Shutdown::stop` end `run`: tasks are
dropped, children killed, sockets removed.

**Runtime:** single-threaded `async-executor` on `async-io` (the smol
family), the runtime zbus uses by default; no tokio. Why: see
[ADR 0022](../../docs/adr/0022-sdk-rust-e-servicos-num-processo.md).

Example: `cargo run -p tessela-service --example counter`.

## License

MIT OR Apache-2.0.
