use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_channel::Sender;
use async_net::unix::UnixStream;
use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::host::{Host, spawn};
use crate::topic::{AnyTopic, Topic};

type Reply = Pin<Box<dyn Future<Output = Result<Value>>>>;
type Handler = Rc<dyn Fn(Value) -> Reply>;

/// A request line longer than this closes the connection.
const MAX_LINE: usize = 1 << 20;

/// One named service: its topics and methods. Serve it with [`Service::run`],
/// or add it to a [`Host`] next to others.
pub struct Service {
    pub(crate) name: String,
    topics: BTreeMap<String, Rc<dyn AnyTopic>>,
    methods: BTreeMap<String, Handler>,
}

/// What `describe` answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Description {
    pub name: String,
    pub paths: Vec<String>,
    pub methods: Vec<String>,
}

/// Method arguments: absent or `null` reads as `{}`, so structs whose fields
/// all have defaults accept a bare call.
fn parse_args<A: DeserializeOwned>(args: Value) -> Result<A> {
    let null = args.is_null();
    serde_json::from_value(args).or_else(|e| {
        if null {
            serde_json::from_value(json!({})).map_err(|_| Error::msg(format!("bad args: {e}")))
        } else {
            Err(Error::msg(format!("bad args: {e}")))
        }
    })
}

impl Service {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            topics: BTreeMap::new(),
            methods: BTreeMap::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Declare a subscribable path with its initial value.
    pub fn topic<T: Serialize + Clone + Send + 'static>(
        &mut self,
        path: &str,
        initial: T,
    ) -> Topic<T> {
        let t = Topic::new(path, initial);
        self.topics.insert(path.to_string(), Rc::new(t.clone()));
        t
    }

    /// Declare an async method. `A` is the argument type (`serde_json::Value`
    /// to take them raw), `R` anything `Serialize` (`()` answers `null`). An
    /// `Err` answers `{"error": message}`. Calls run concurrently, so a slow
    /// one never holds up other requests on the same connection.
    pub fn method<A, R, F, Fut>(&mut self, name: &str, f: F) -> &mut Self
    where
        A: DeserializeOwned + 'static,
        R: Serialize + 'static,
        F: Fn(A) -> Fut + 'static,
        Fut: Future<Output = Result<R>> + 'static,
    {
        let f = Rc::new(f);
        let handler: Handler = Rc::new(move |args: Value| {
            let f = f.clone();
            Box::pin(async move {
                let r = f(parse_args(args)?).await?;
                Ok(serde_json::to_value(r)?)
            })
        });
        self.methods.insert(name.to_string(), handler);
        self
    }

    /// Declare a method that answers right away.
    pub fn method_sync<A, R, F>(&mut self, name: &str, f: F) -> &mut Self
    where
        A: DeserializeOwned + 'static,
        R: Serialize + 'static,
        F: Fn(A) -> Result<R> + 'static,
    {
        let handler: Handler = Rc::new(move |args: Value| {
            let r = parse_args(args)
                .and_then(&f)
                .and_then(|r| Ok(serde_json::to_value(r)?));
            Box::pin(async move { r })
        });
        self.methods.insert(name.to_string(), handler);
        self
    }

    pub fn describe(&self) -> Description {
        Description {
            name: self.name.clone(),
            paths: self.topics.keys().cloned().collect(),
            methods: self.methods.keys().cloned().collect(),
        }
    }

    /// Serve only this service until SIGTERM/SIGINT; `background` runs beside it.
    pub fn run(self, background: impl Future<Output = ()> + 'static) -> std::io::Result<()> {
        Host::new().add(self).run(background)
    }
}

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// How Python's f-strings print a request field: strings bare, a missing one
/// as `None`, anything else as JSON. Error texts match the Python SDK.
fn shown(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn line(id: &Value, key: &str, v: &Value) -> String {
    format!("{{\"id\":{id},\"{key}\":{v}}}\n")
}

impl Service {
    pub(crate) async fn serve_client(self: Rc<Self>, stream: UnixStream) {
        let client = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = async_channel::unbounded::<String>();
        let mut out = stream.clone();
        let writer = spawn(async move {
            while let Ok(l) = rx.recv().await {
                if out.write_all(l.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let mut reader = BufReader::new(stream);
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) if buf.len() > MAX_LINE => break,
                Ok(_) => {}
            }
            match serde_json::from_str::<Value>(&buf) {
                Ok(Value::Object(req)) => self.dispatch(client, &tx, req),
                Ok(_) => {
                    let _ = tx.try_send(line(
                        &json!(0),
                        "error",
                        &json!("bad request: not an object"),
                    ));
                }
                Err(e) => {
                    let _ = tx.try_send(line(
                        &json!(0),
                        "error",
                        &json!(format!("bad request: {e}")),
                    ));
                }
            }
        }
        for t in self.topics.values() {
            t.drop_client(client);
        }
        drop(tx);
        // Calls still running hold a sender; the writer ends after them.
        writer.detach();
    }

    fn dispatch(&self, client: u64, tx: &Sender<String>, req: serde_json::Map<String, Value>) {
        let id = req.get("id").cloned().unwrap_or(json!(0));
        let send = |key: &str, v: Value| {
            let _ = tx.try_send(line(&id, key, &v));
        };
        let op = req.get("op");
        match op.and_then(Value::as_str) {
            Some(op @ ("get" | "subscribe")) => {
                let path = req.get("path");
                let topic = path
                    .and_then(Value::as_str)
                    .and_then(|p| self.topics.get(p));
                match topic {
                    None => send("error", json!(format!("unknown path {}", shown(path)))),
                    Some(t) if op == "get" => send("result", t.current()),
                    Some(t) => t.subscribe(client, id.clone(), tx),
                }
            }
            Some("unsubscribe") => {
                let sub = req.get("sub").cloned().unwrap_or(Value::Null);
                for t in self.topics.values() {
                    t.unsubscribe(client, &sub);
                }
                send("result", Value::Null);
            }
            Some("call") => {
                let method = req.get("method");
                let Some(handler) = method
                    .and_then(Value::as_str)
                    .and_then(|m| self.methods.get(m))
                else {
                    send("error", json!(format!("unknown method {}", shown(method))));
                    return;
                };
                let reply = handler(req.get("args").cloned().unwrap_or(Value::Null));
                let tx = tx.clone();
                spawn(async move {
                    let l = match reply.await {
                        Ok(v) => line(&id, "result", &v),
                        Err(e) => line(&id, "error", &json!(e.message())),
                    };
                    let _ = tx.try_send(l);
                })
                .detach();
            }
            Some("describe") => send("result", json!(self.describe())),
            _ => send("error", json!(format!("unknown op {}", shown(op)))),
        }
    }
}
