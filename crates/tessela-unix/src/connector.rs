//! The universal connector.
//!
//! The shell core owns every socket. Widgets never open one: they ask the
//! connector, by *service name*, for a subscription or a call, and the
//! connector checks nothing else — permission checks happen one layer up, in
//! the runtime, against the widget manifest.
//!
//! What the connector buys you:
//! - **Multiplexing**: ten widgets watching `audio/sink.default` share one
//!   subscription on the wire; late joiners get the cached last value at once.
//! - **Resilience**: services may start late, crash and come back. Interest is
//!   remembered and re-subscribed on reconnect; listeners see `Unavailable`
//!   while the service is down.
//! - **Discovery**: a service is just `<dir>/<name>.sock`, or an explicit
//!   `name = "/path/to.sock"` entry for sockets that live elsewhere (niri, ...).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select, tick, unbounded};
use serde_json::Value;
use tessela_proto::{self as proto, Op, Request, Response};

/// Caller-chosen correlation token, echoed back in every [`Delivery`].
pub type Token = u64;

#[derive(Debug, Clone, PartialEq)]
pub enum Delivery {
    Event {
        token: Token,
        value: Value,
    },
    /// The service backing this subscription is not reachable (yet).
    Unavailable {
        token: Token,
    },
    CallResult {
        token: Token,
        result: Result<Value, String>,
    },
}

/// Where deliveries for one listener go. Returning `false` means the
/// listener is gone and can be dropped.
#[derive(Clone)]
pub struct DeliverySink(Arc<dyn Fn(Delivery) -> bool + Send + Sync>);

impl DeliverySink {
    pub fn new(f: impl Fn(Delivery) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
    fn send(&self, d: Delivery) -> Result<(), ()> {
        if (self.0)(d) { Ok(()) } else { Err(()) }
    }
}

impl From<Sender<Delivery>> for DeliverySink {
    fn from(tx: Sender<Delivery>) -> Self {
        Self::new(move |d| tx.send(d).is_ok())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub dirs: Vec<PathBuf>,
    pub explicit: HashMap<String, PathBuf>,
}

/// Service names become file names: keep them boring so a widget can never
/// name `../../run/user/1000/bus` and reach an arbitrary socket.
pub fn valid_service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

impl Registry {
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        if !valid_service_name(name) {
            return None;
        }
        if let Some(p) = self.explicit.get(name) {
            return Some(p.clone());
        }
        self.dirs
            .iter()
            .map(|d| d.join(format!("{name}.sock")))
            .find(|p| p.exists())
    }

    pub fn known(&self) -> Vec<String> {
        let mut names: Vec<String> = self.explicit.keys().cloned().collect();
        for dir in &self.dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for e in entries.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(stem) = n.strip_suffix(".sock") {
                    if valid_service_name(stem) && !names.iter().any(|x| x == stem) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
        names.sort();
        names
    }
}

enum Cmd {
    Subscribe {
        service: String,
        path: String,
        sink: DeliverySink,
        token: Token,
    },
    Unsubscribe {
        service: String,
        path: String,
        token: Token,
    },
    Call {
        service: String,
        method: String,
        args: Value,
        sink: DeliverySink,
        token: Token,
    },
    Incoming {
        service: String,
        generation: u64,
        resp: Response,
    },
    Closed {
        service: String,
        generation: u64,
    },
}

#[derive(Clone)]
pub struct Connector {
    tx: Sender<Cmd>,
}

impl Connector {
    pub fn start(registry: Registry) -> Self {
        let (tx, rx) = unbounded();
        let me = tx.clone();
        thread::Builder::new()
            .name("connector".into())
            .spawn(move || Manager::new(registry, me).run(rx))
            .expect("spawn connector thread");
        Self { tx }
    }

    pub fn subscribe(
        &self,
        service: &str,
        path: &str,
        sink: impl Into<DeliverySink>,
        token: Token,
    ) {
        let sink = sink.into();
        let _ = self.tx.send(Cmd::Subscribe {
            service: service.into(),
            path: path.into(),
            sink,
            token,
        });
    }

    pub fn unsubscribe(&self, service: &str, path: &str, token: Token) {
        let _ = self.tx.send(Cmd::Unsubscribe {
            service: service.into(),
            path: path.into(),
            token,
        });
    }

    pub fn call(
        &self,
        service: &str,
        method: &str,
        args: Value,
        sink: impl Into<DeliverySink>,
        token: Token,
    ) {
        let sink = sink.into();
        let _ = self.tx.send(Cmd::Call {
            service: service.into(),
            method: method.into(),
            args,
            sink,
            token,
        });
    }
}

struct Topic {
    wire_id: Option<u64>,
    last: Option<Value>,
    listeners: Vec<(DeliverySink, Token)>,
}

struct Service {
    conn: Option<UnixStream>,
    generation: u64,
    next_retry: Instant,
    backoff: Duration,
    topics: HashMap<String, Topic>,
    /// wire id -> topic path
    wire_topics: HashMap<u64, String>,
    pending_calls: HashMap<u64, (DeliverySink, Token)>,
}

impl Service {
    fn new() -> Self {
        Self {
            conn: None,
            generation: 0,
            next_retry: Instant::now(),
            backoff: Duration::from_millis(250),
            topics: HashMap::new(),
            wire_topics: HashMap::new(),
            pending_calls: HashMap::new(),
        }
    }
}

struct Manager {
    registry: Registry,
    me: Sender<Cmd>,
    services: HashMap<String, Service>,
    next_id: u64,
}

impl Manager {
    fn new(registry: Registry, me: Sender<Cmd>) -> Self {
        Self {
            registry,
            me,
            services: HashMap::new(),
            next_id: 1,
        }
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        let ticker = tick(Duration::from_millis(200));
        loop {
            select! {
                recv(rx) -> cmd => match cmd {
                    Ok(cmd) => self.handle(cmd),
                    Err(_) => return,
                },
                recv(ticker) -> _ => self.reconnect_due(),
            }
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Subscribe {
                service,
                path,
                sink,
                token,
            } => {
                let svc = self
                    .services
                    .entry(service.clone())
                    .or_insert_with(Service::new);
                let connected = svc.conn.is_some();
                let topic = svc.topics.entry(path.clone()).or_insert_with(|| Topic {
                    wire_id: None,
                    last: None,
                    listeners: Vec::new(),
                });
                // Late joiner: replay the cached value instead of a new wire subscribe.
                if let Some(v) = &topic.last {
                    let _ = sink.send(Delivery::Event {
                        token,
                        value: v.clone(),
                    });
                }
                topic.listeners.push((sink.clone(), token));
                let needs_wire = topic.wire_id.is_none();

                if !connected {
                    // Connecting re-subscribes every remembered topic, this one included.
                    self.try_connect(&service);
                    if self.services[&service].conn.is_none() {
                        let _ = sink.send(Delivery::Unavailable { token });
                    }
                } else if needs_wire {
                    self.send_subscribe(&service, &path);
                }
            }
            Cmd::Unsubscribe {
                service,
                path,
                token,
            } => {
                let Some(svc) = self.services.get_mut(&service) else {
                    return;
                };
                let Some(topic) = svc.topics.get_mut(&path) else {
                    return;
                };
                topic.listeners.retain(|(_, t)| *t != token);
                if topic.listeners.is_empty() {
                    let wire = topic.wire_id;
                    svc.topics.remove(&path);
                    if let Some(w) = wire {
                        svc.wire_topics.remove(&w);
                        let id = self.id();
                        self.write(
                            &service,
                            Request {
                                id,
                                op: Op::Unsubscribe { sub: w },
                            },
                        );
                    }
                }
            }
            Cmd::Call {
                service,
                method,
                args,
                sink,
                token,
            } => {
                if !self
                    .services
                    .get(&service)
                    .is_some_and(|s| s.conn.is_some())
                {
                    self.services
                        .entry(service.clone())
                        .or_insert_with(Service::new);
                    self.try_connect(&service);
                }
                if !self.services[&service].conn.is_some() {
                    let _ = sink.send(Delivery::CallResult {
                        token,
                        result: Err(format!("service '{service}' unavailable")),
                    });
                    return;
                }
                let id = self.id();
                self.services
                    .get_mut(&service)
                    .unwrap()
                    .pending_calls
                    .insert(id, (sink, token));
                self.write(
                    &service,
                    Request {
                        id,
                        op: Op::Call { method, args },
                    },
                );
            }
            Cmd::Incoming {
                service,
                generation,
                resp,
            } => self.incoming(&service, generation, resp),
            Cmd::Closed {
                service,
                generation,
            } => self.closed(&service, generation),
        }
    }

    fn incoming(&mut self, service: &str, generation: u64, resp: Response) {
        let Some(svc) = self.services.get_mut(service) else {
            return;
        };
        if svc.generation != generation {
            return;
        }
        if let Some(value) = resp.event {
            let Some(path) = svc.wire_topics.get(&resp.id) else {
                return;
            };
            if let Some(topic) = svc.topics.get_mut(path) {
                topic.last = Some(value.clone());
                topic.listeners.retain(|(sink, token)| {
                    sink.send(Delivery::Event {
                        token: *token,
                        value: value.clone(),
                    })
                    .is_ok()
                });
            }
        } else if let Some((sink, token)) = svc.pending_calls.remove(&resp.id) {
            let result = match resp.error {
                Some(e) => Err(e),
                None => Ok(resp.result.unwrap_or(Value::Null)),
            };
            let _ = sink.send(Delivery::CallResult { token, result });
        } else if let Some(e) = resp.error {
            // An error on a subscription id: the path does not exist.
            if let Some(path) = svc.wire_topics.get(&resp.id) {
                log::warn!("{service}: subscription to '{path}' rejected: {e}");
            }
        }
    }

    fn closed(&mut self, service: &str, generation: u64) {
        let Some(svc) = self.services.get_mut(service) else {
            return;
        };
        if svc.generation != generation {
            return;
        }
        log::info!("service '{service}' disconnected");
        svc.conn = None;
        svc.wire_topics.clear();
        svc.next_retry = Instant::now() + svc.backoff;
        for topic in svc.topics.values_mut() {
            topic.wire_id = None;
            topic.last = None;
            for (sink, token) in &topic.listeners {
                let _ = sink.send(Delivery::Unavailable { token: *token });
            }
        }
        for (_, (sink, token)) in svc.pending_calls.drain() {
            let _ = sink.send(Delivery::CallResult {
                token,
                result: Err("service disconnected".into()),
            });
        }
    }

    fn reconnect_due(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .services
            .iter()
            .filter(|(_, s)| s.conn.is_none() && !s.topics.is_empty() && s.next_retry <= now)
            .map(|(n, _)| n.clone())
            .collect();
        for name in due {
            self.try_connect(&name);
        }
    }

    fn try_connect(&mut self, name: &str) {
        let path = self.registry.resolve(name);
        let svc = self.services.get_mut(name).expect("service entry exists");
        let stream = path.as_ref().and_then(|p| match UnixStream::connect(p) {
            Ok(s) => Some(s),
            Err(e) => {
                // Absent or not yet listening is normal (the service may start
                // later); anything else is a configuration problem worth saying.
                use std::io::ErrorKind::{ConnectionRefused, NotFound};
                if !matches!(e.kind(), NotFound | ConnectionRefused)
                    && svc.backoff <= Duration::from_millis(250)
                {
                    log::warn!("service '{name}': cannot connect to {}: {e}", p.display());
                }
                None
            }
        });
        let Some(stream) = stream else {
            svc.next_retry = Instant::now() + svc.backoff;
            svc.backoff = (svc.backoff * 2).min(Duration::from_secs(5));
            return;
        };
        log::info!("service '{name}' connected");
        svc.generation += 1;
        svc.backoff = Duration::from_millis(250);
        let generation = svc.generation;
        let reader = stream.try_clone().expect("clone unix stream");
        svc.conn = Some(stream);

        let me = self.me.clone();
        let service = name.to_string();
        thread::Builder::new()
            .name(format!("svc-{name}"))
            .spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Response>(&line) {
                        Ok(resp) => {
                            if me
                                .send(Cmd::Incoming {
                                    service: service.clone(),
                                    generation,
                                    resp,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) => log::warn!("{service}: bad line {line:?}: {e}"),
                    }
                }
                let _ = me.send(Cmd::Closed {
                    service,
                    generation,
                });
            })
            .expect("spawn reader");

        let paths: Vec<String> = self.services[name].topics.keys().cloned().collect();
        for p in paths {
            self.send_subscribe(name, &p);
        }
    }

    fn send_subscribe(&mut self, service: &str, path: &str) {
        let id = self.id();
        let svc = self.services.get_mut(service).unwrap();
        if let Some(topic) = svc.topics.get_mut(path) {
            topic.wire_id = Some(id);
            svc.wire_topics.insert(id, path.to_string());
        }
        self.write(
            service,
            Request {
                id,
                op: Op::Subscribe { path: path.into() },
            },
        );
    }

    fn write(&mut self, service: &str, req: Request) {
        let Some(svc) = self.services.get_mut(service) else {
            return;
        };
        let Some(conn) = svc.conn.as_mut() else {
            return;
        };
        if conn.write_all(proto::encode(&req).as_bytes()).is_err() {
            let generation = svc.generation;
            self.closed(service, generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// Minimal in-test service: answers subscribe with two events, echoes calls.
    fn fake_service(dir: &std::path::Path, name: &str) -> thread::JoinHandle<usize> {
        let listener = UnixListener::bind(dir.join(format!("{name}.sock"))).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut w = stream.try_clone().unwrap();
            let mut subscribes = 0;
            for line in BufReader::new(stream).lines() {
                let req: Request = serde_json::from_str(&line.unwrap()).unwrap();
                match req.op {
                    Op::Subscribe { .. } => {
                        subscribes += 1;
                        w.write_all(proto::encode(&Response::event(req.id, 1.into())).as_bytes())
                            .unwrap();
                        w.write_all(proto::encode(&Response::event(req.id, 2.into())).as_bytes())
                            .unwrap();
                    }
                    Op::Call { args, .. } => {
                        w.write_all(proto::encode(&Response::result(req.id, args)).as_bytes())
                            .unwrap();
                        return subscribes;
                    }
                    _ => {}
                }
            }
            subscribes
        })
    }

    #[test]
    fn multiplexes_subscriptions_and_routes_calls() {
        let dir = std::env::temp_dir().join(format!("marathon-conn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = fake_service(&dir, "fake");
        let conn = Connector::start(Registry {
            dirs: vec![dir.clone()],
            ..Default::default()
        });

        let (tx, rx) = unbounded();
        conn.subscribe("fake", "n", tx.clone(), 10);
        let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            first,
            Delivery::Event {
                token: 10,
                value: 1.into()
            }
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Delivery::Event {
                token: 10,
                value: 2.into()
            }
        );

        // Second listener on the same path: served from cache, no new wire subscribe.
        conn.subscribe("fake", "n", tx.clone(), 11);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Delivery::Event {
                token: 11,
                value: 2.into()
            }
        );

        conn.call("fake", "echo", serde_json::json!({"x": 1}), tx, 12);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Delivery::CallResult {
                token: 12,
                result: Ok(serde_json::json!({"x": 1}))
            }
        );
        assert_eq!(server.join().unwrap(), 1, "exactly one wire subscription");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_path_like_service_names() {
        let reg = Registry {
            dirs: vec!["/tmp".into()],
            ..Default::default()
        };
        assert!(reg.resolve("../etc/passwd").is_none());
        assert!(reg.resolve("Audio").is_none());
        assert!(valid_service_name("niri-ipc"));
    }

    #[test]
    fn unknown_service_reports_unavailable() {
        let conn = Connector::start(Registry::default());
        let (tx, rx) = unbounded();
        conn.subscribe("ghost", "x", tx.clone(), 1);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Delivery::Unavailable { token: 1 }
        );
        conn.call("ghost", "m", Value::Null, tx, 2);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Delivery::CallResult {
                token: 2,
                result: Err(_)
            }
        ));
    }
}
