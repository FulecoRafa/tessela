//! A counter service: `count` goes up every second, `reset` sets it back.
//!
//!     cargo run -p tessela-service --example counter
//!     socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/marathon/services/counter.sock
//!     {"id":1,"op":"subscribe","path":"count"}
//!     {"id":2,"op":"call","method":"reset","args":{"to":10}}

use std::time::Duration;

use serde::Deserialize;
use tessela_service::{Service, Timer};

#[derive(Deserialize)]
struct Reset {
    #[serde(default)]
    to: u64,
}

fn main() -> std::io::Result<()> {
    let mut svc = Service::new("counter");
    let count = svc.topic("count", 0u64);

    let c = count.clone();
    svc.method_sync("reset", move |args: Reset| {
        c.publish(args.to);
        Ok(args.to)
    });

    svc.run(async move {
        loop {
            Timer::after(Duration::from_secs(1)).await;
            count.update(|n| *n += 1);
        }
    })
}
