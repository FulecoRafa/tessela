//! The protocol over a real socket, with a plain blocking client.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tessela_service::{Host, Service, Shutdown, Timer};

fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "tessela-service-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Run the services `build` makes on their own thread; wait for the sockets.
fn start(
    dir: &Path,
    names: &[&str],
    build: impl FnOnce() -> Vec<Service> + Send + 'static,
) -> (Shutdown, thread::JoinHandle<std::io::Result<()>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let d = dir.to_path_buf();
    let handle = thread::spawn(move || {
        let mut host = Host::new().dir(d).keep_running_without_parent();
        for s in build() {
            host = host.add(s);
        }
        tx.send(host.shutdown_handle()).unwrap();
        host.run(async {})
    });
    let stop = rx.recv().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    for n in names {
        while UnixStream::connect(dir.join(format!("{n}.sock"))).is_err() {
            assert!(Instant::now() < deadline, "{n}.sock never came up");
            thread::sleep(Duration::from_millis(10));
        }
    }
    (stop, handle)
}

struct Client {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl Client {
    fn new(dir: &Path, name: &str) -> Self {
        let w = UnixStream::connect(dir.join(format!("{name}.sock"))).unwrap();
        w.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let r = BufReader::new(w.try_clone().unwrap());
        Self { w, r }
    }
    fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).unwrap();
    }
    fn raw(&mut self) -> String {
        let mut l = String::new();
        self.r.read_line(&mut l).unwrap();
        l
    }
    fn recv(&mut self) -> Value {
        serde_json::from_str(&self.raw()).unwrap()
    }
    fn ask(&mut self, v: Value) -> Value {
        self.send(v);
        self.recv()
    }
}

fn demo() -> Service {
    let mut s = Service::new("demo");
    let level = s.topic("level", json!({"value": 1}));
    let l = level.clone();
    s.method_sync("set", move |args: Value| {
        l.publish(json!({"value": args["value"]}));
        Ok(())
    });
    s.method("slow", |_: Value| async {
        Timer::after(Duration::from_millis(300)).await;
        Ok("late")
    });
    s.method_sync("fail", |_: Value| -> tessela_service::Result<()> {
        Err("nope".into())
    });
    s
}

#[test]
fn get_call_describe_round_trip() {
    let dir = temp_dir();
    let (stop, h) = start(&dir, &["demo"], || vec![demo()]);
    let mut c = Client::new(&dir, "demo");
    assert_eq!(
        c.ask(json!({"id":1,"op":"get","path":"level"})),
        json!({"id":1,"result":{"value":1}})
    );
    assert_eq!(
        c.ask(json!({"id":2,"op":"call","method":"set","args":{"value":5}})),
        json!({"id":2,"result":null})
    );
    assert_eq!(
        c.ask(json!({"id":3,"op":"get","path":"level"})),
        json!({"id":3,"result":{"value":5}})
    );
    assert_eq!(
        c.ask(json!({"id":4,"op":"call","method":"fail"})),
        json!({"id":4,"error":"nope"})
    );
    assert_eq!(
        c.ask(json!({"id":5,"op":"describe"})),
        json!({"id":5,"result":{"name":"demo","paths":["level"],"methods":["fail","set","slow"]}})
    );
    stop.stop();
    h.join().unwrap().unwrap();
    assert!(
        !dir.join("demo.sock").exists(),
        "socket removed on shutdown"
    );
}

#[test]
fn errors_read_like_the_python_sdk() {
    let dir = temp_dir();
    let (stop, h) = start(&dir, &["demo"], || vec![demo()]);
    let mut c = Client::new(&dir, "demo");
    // Byte for byte what sdk/python/marathon_service.py writes.
    c.send(json!({"id":7,"op":"call","method":"nope"}));
    assert_eq!(c.raw(), "{\"id\":7,\"error\":\"unknown method nope\"}\n");
    c.send(json!({"id":8,"op":"get","path":"nope"}));
    assert_eq!(c.raw(), "{\"id\":8,\"error\":\"unknown path nope\"}\n");
    c.send(json!({"id":9,"op":"frobnicate"}));
    assert_eq!(c.raw(), "{\"id\":9,\"error\":\"unknown op frobnicate\"}\n");
    c.send(json!({"id":10,"op":"call"}));
    assert_eq!(c.raw(), "{\"id\":10,\"error\":\"unknown method None\"}\n");
    c.w.write_all(b"not json\n").unwrap();
    let bad = c.recv();
    assert_eq!(bad["id"], 0);
    assert!(bad["error"].as_str().unwrap().starts_with("bad request: "));
    // Still serving after all that.
    assert_eq!(c.ask(json!({"id":11,"op":"get","path":"level"}))["id"], 11);
    stop.stop();
    h.join().unwrap().unwrap();
}

#[test]
fn subscription_gets_the_current_value_then_changes_once_each() {
    let dir = temp_dir();
    let (stop, h) = start(&dir, &["demo"], || vec![demo()]);
    let mut sub = Client::new(&dir, "demo");
    let mut ctl = Client::new(&dir, "demo");
    assert_eq!(
        sub.ask(json!({"id":1,"op":"subscribe","path":"level"})),
        json!({"id":1,"event":{"value":1}})
    );
    ctl.ask(json!({"id":1,"op":"call","method":"set","args":{"value":2}}));
    // Equal value: deduplicated, no event.
    ctl.ask(json!({"id":2,"op":"call","method":"set","args":{"value":2}}));
    ctl.ask(json!({"id":3,"op":"call","method":"set","args":{"value":3}}));
    assert_eq!(sub.recv(), json!({"id":1,"event":{"value":2}}));
    assert_eq!(sub.recv(), json!({"id":1,"event":{"value":3}}));
    // After unsubscribing, nothing more arrives on that id.
    assert_eq!(
        sub.ask(json!({"id":2,"op":"unsubscribe","sub":1})),
        json!({"id":2,"result":null})
    );
    ctl.ask(json!({"id":4,"op":"call","method":"set","args":{"value":4}}));
    assert_eq!(
        sub.ask(json!({"id":3,"op":"get","path":"level"})),
        json!({"id":3,"result":{"value":4}})
    );
    stop.stop();
    h.join().unwrap().unwrap();
}

#[test]
fn a_slow_call_does_not_hold_up_the_connection() {
    let dir = temp_dir();
    let (stop, h) = start(&dir, &["demo"], || vec![demo()]);
    let mut c = Client::new(&dir, "demo");
    c.send(json!({"id":1,"op":"call","method":"slow"}));
    c.send(json!({"id":2,"op":"get","path":"level"}));
    assert_eq!(c.recv()["id"], 2);
    assert_eq!(c.recv(), json!({"id":1,"result":"late"}));
    stop.stop();
    h.join().unwrap().unwrap();
}

#[test]
fn several_services_share_one_process_and_state() {
    let dir = temp_dir();
    let (stop, h) = start(&dir, &["audio", "power"], || {
        let mut audio = Service::new("audio");
        let volume = audio.topic("volume", 0.5);
        let mut power = Service::new("power");
        power.topic("profile", "balanced");
        // A method of one service publishing on another's topic.
        power.method_sync("mute_all", move |_: Value| {
            volume.publish(0.0);
            Ok(())
        });
        vec![audio, power]
    });
    let mut a = Client::new(&dir, "audio");
    let mut p = Client::new(&dir, "power");
    assert_eq!(
        a.ask(json!({"id":1,"op":"subscribe","path":"volume"})),
        json!({"id":1,"event":0.5})
    );
    assert_eq!(
        p.ask(json!({"id":1,"op":"get","path":"profile"})),
        json!({"id":1,"result":"balanced"})
    );
    assert_eq!(
        p.ask(json!({"id":2,"op":"get","path":"volume"}))["error"],
        "unknown path volume"
    );
    p.ask(json!({"id":3,"op":"call","method":"mute_all"}));
    assert_eq!(a.recv(), json!({"id":1,"event":0.0}));
    stop.stop();
    h.join().unwrap().unwrap();
    assert!(!dir.join("audio.sock").exists() && !dir.join("power.sock").exists());
}

#[test]
fn stale_sockets_are_replaced_live_ones_are_not() {
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    // A socket file nobody listens on (a crashed run).
    drop(UnixListener::bind(dir.join("stale.sock")).unwrap());
    let (stop, h) = start(&dir, &["stale"], || vec![Service::new("stale")]);
    let mode = std::fs::metadata(dir.join("stale.sock"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // A second process wanting the same name fails instead of stealing it.
    let d = dir.clone();
    let second = thread::spawn(move || {
        Host::new()
            .dir(d)
            .keep_running_without_parent()
            .add(Service::new("stale"))
            .run(async {})
    })
    .join()
    .unwrap();
    assert_eq!(second.unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
    assert!(UnixStream::connect(dir.join("stale.sock")).is_ok());
    stop.stop();
    h.join().unwrap().unwrap();
}

#[test]
fn typed_topics_and_arguments() {
    #[derive(serde::Serialize, Clone)]
    struct Caffeine {
        on: bool,
        minutes: u32,
    }
    #[derive(serde::Deserialize)]
    struct On {
        #[serde(default)]
        minutes: u32,
    }
    let dir = temp_dir();
    let (stop, h) = start(&dir, &["power"], || {
        let mut s = Service::new("power");
        let caf = s.topic(
            "caffeine",
            Caffeine {
                on: false,
                minutes: 0,
            },
        );
        s.method("caffeine", move |a: On| {
            let caf = caf.clone();
            async move {
                caf.publish(Caffeine {
                    on: true,
                    minutes: a.minutes,
                });
                Ok(caf.get().minutes)
            }
        });
        vec![s]
    });
    let mut c = Client::new(&dir, "power");
    // No args at all: the struct's defaults.
    assert_eq!(
        c.ask(json!({"id":1,"op":"call","method":"caffeine"})),
        json!({"id":1,"result":0})
    );
    assert_eq!(
        c.ask(json!({"id":2,"op":"call","method":"caffeine","args":{"minutes":30}})),
        json!({"id":2,"result":30})
    );
    assert_eq!(
        c.ask(json!({"id":3,"op":"get","path":"caffeine"})),
        json!({"id":3,"result":{"on":true,"minutes":30}})
    );
    let bad = c.ask(json!({"id":4,"op":"call","method":"caffeine","args":{"minutes":"x"}}));
    assert!(bad["error"].as_str().unwrap().starts_with("bad args"));
    stop.stop();
    h.join().unwrap().unwrap();
}
