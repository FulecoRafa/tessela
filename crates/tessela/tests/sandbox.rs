//! Behaviour every engine must share, and the sandbox guarantees, tested
//! through the public API with real engines and a real socket module.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use serde_json::{Value, json};
use tessela::core::{
    FnId, Grants, Handler, Inbound, Instance, InstanceSpec, Limits, Module, Node, PersistStore,
    Sink, Vocabulary,
};
use tessela::proto::{Op, Request, Response, encode};
use tessela::unix::UnixServices;
use tessela::view::ERROR;

fn limits() -> Limits {
    Limits {
        memory_bytes: 8 << 20,
        budget: Duration::from_millis(100),
        ..Limits::default()
    }
}

struct Harness {
    inst: Instance,
    inbound: Receiver<Inbound>,
}

fn text(s: &str) -> Node {
    Node::new("Text").prop("text", s)
}

fn fn_id(n: &Node, prop: &str) -> Option<FnId> {
    match n.handler(prop) {
        Some(Handler::Fn(id)) => Some(id),
        _ => None,
    }
}

impl Harness {
    fn try_new(
        file: &str,
        src: &str,
        caps: &[&str],
        modules: Vec<Arc<dyn Module>>,
    ) -> Result<Self, String> {
        Self::with(file, src, caps, modules, None)
    }

    fn with(
        file: &str,
        src: &str,
        caps: &[&str],
        modules: Vec<Arc<dyn Module>>,
        persist: Option<PersistStore>,
    ) -> Result<Self, String> {
        let (tx, inbound) = unbounded();
        let inst = Instance::new(InstanceSpec {
            name: "test",
            file,
            source: src,
            grants: Grants::new(caps.iter().map(|s| s.to_string())),
            limits: limits(),
            engines: &tessela::engines(),
            modules,
            vocabulary: Arc::new(Vocabulary::default()),
            sink: Sink::new(move |i| {
                let _ = tx.send(i);
            }),
            persist,
            sources: None,
            trace: None,
        })
        .map_err(|e| e.0)?;
        Ok(Self { inst, inbound })
    }

    fn new(file: &str, src: &str, caps: &[&str], modules: Vec<Arc<dyn Module>>) -> Self {
        Self::try_new(file, src, caps, modules).unwrap_or_else(|e| panic!("load failed: {e}"))
    }

    fn plain(file: &str, src: &str) -> Self {
        Self::new(file, src, &[], vec![])
    }

    /// Deliver module inputs until `done(render)` holds or the timeout passes.
    fn pump_until(&mut self, mut done: impl FnMut(&Node) -> bool) -> Node {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let node = self.inst.render();
            if done(&node) {
                return node;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.inbound.recv_timeout(left) {
                Ok(i) => {
                    let _ = self.inst.deliver(i);
                }
                Err(_) => panic!("timed out; last render: {node:?}"),
            }
        }
    }

    fn logs(&mut self) -> Vec<String> {
        self.inst.take_logs()
    }
}

fn load_err(file: &str, src: &str) -> String {
    match Harness::try_new(file, src, &[], vec![]) {
        Ok(mut h) => match h.inst.render() {
            n if n.is(ERROR) => n.str("message").unwrap_or("").to_string(),
            other => panic!("expected an error, rendered {other:?}"),
        },
        Err(e) => e,
    }
}

fn pill(node: &Node) -> (&str, Option<FnId>) {
    match node.kind.as_str() {
        "Pill" => (node.str("label").unwrap(), fn_id(node, "on_click")),
        "Row" => pill(&node.children[0]),
        _ => panic!("not a pill: {node:?}"),
    }
}

// ---------------------------------------------------------------- reactivity

const LUA_COUNTER: &str = r#"
local n = signal(0)
return Row {
  gap = 4,
  Pill {
    icon = "plus",
    label = function() log("label"); return "clicks: " .. n() end,
    on_click = function() n:update(function(v) return v + 1 end) end,
  },
  Text { text = "static", tone = "muted" },
}
"#;

const TSX_COUNTER: &str = r#"
export default function Counter() {
  const n = signal<number>(0);
  return (
    <Row gap={4}>
      <Pill icon="plus"
            label={() => { log("label"); return `clicks: ${n()}`; }}
            onClick={() => n.update((v: number) => v + 1)} />
      <Text text="static" tone="muted" />
    </Row>
  );
}
"#;

fn counter_behaves(file: &str, src: &str) {
    let mut h = Harness::plain(file, src);
    let tree = h.inst.render();
    let (label, click) = pill(&tree);
    assert_eq!(label, "clicks: 0");
    assert!(tree.is("Row"));
    assert_eq!(tree.f32("gap"), Some(4.0));
    assert_eq!(tree.children[1], text("static").prop("tone", "muted"));
    assert_eq!(h.logs(), ["label"]);

    // Nothing changed: re-render must not re-enter the guest.
    h.inst.render();
    assert!(h.logs().is_empty(), "clean bindings are cached");

    h.inst.ui(click.unwrap(), Value::Null).unwrap();
    assert!(h.inst.needs_render());
    assert_eq!(pill(&h.inst.render()).0, "clicks: 1");
    assert_eq!(h.logs(), ["label"], "only the dependent binding re-ran");
}

#[test]
fn lua_counter() {
    counter_behaves("counter.luau", LUA_COUNTER);
}

#[test]
fn tsx_counter() {
    counter_behaves("counter.tsx", TSX_COUNTER);
}

#[test]
fn whole_widget_function_rerenders_react_style() {
    let mut h = Harness::plain(
        "w.luau",
        r#"
local on = signal(false)
return function()
  return Pill { label = on() and "on" or "off", active = on(), on_click = function() on:set(not on:peek()) end }
end
"#,
    );
    let tree = h.inst.render();
    let (label, click) = pill(&tree);
    assert_eq!(label, "off");
    let click = click.unwrap();
    h.inst.ui(click, Value::Null).unwrap();
    assert!(h.inst.render().bool("active"));
    // The old click handler was owned by the previous render and is gone.
    assert!(h.inst.ui(click, Value::Null).is_err());
}

#[test]
fn rebuilding_lists_does_not_leak() {
    let mut h = Harness::plain(
        "list.tsx",
        r#"
const items = signal<number[]>([1, 2, 3]);
export default () => (
  <Column>
    {() => items().map((i: number) => <Button label={() => `#${i}`} onClick={() => items.set([...items.peek(), i])} />)}
  </Column>
);
"#,
    );
    h.inst.render();
    assert_eq!(h.inst.live_counts(), (3 + 1, 1));
    for _ in 0..50 {
        let handler = first_button(&h.inst.render());
        h.inst.ui(handler, Value::Null).unwrap();
        h.inst.render();
    }
    assert_eq!(h.inst.render().children.len(), 53);
    // One label binding per item plus the list binding; nothing from older renders.
    assert_eq!(h.inst.live_counts(), (53 + 1, 1));
}

fn first_button(n: &Node) -> FnId {
    match n.kind.as_str() {
        "Column" => first_button(&n.children[0]),
        "Button" => fn_id(n, "on_click").unwrap(),
        _ => panic!("{n:?}"),
    }
}

// ---------------------------------------------------------------- modules: service over a real socket

/// A one-connection `power` service. Records every op it receives.
fn power_service(dir: &std::path::Path) -> Receiver<String> {
    let listener = UnixListener::bind(dir.join("power.sock")).unwrap();
    let (ops_tx, ops) = unbounded();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut w = stream.try_clone().unwrap();
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let req: Request = serde_json::from_str(&line).unwrap();
            let reply = match &req.op {
                Op::Subscribe { .. } => {
                    Response::event(req.id, json!({"percent": 87, "charging": true}))
                }
                Op::Call { method, args } if method == "set_profile" => {
                    Response::result(req.id, args["profile"].clone())
                }
                Op::Call { method, .. } if method == "hang_up" => return, // drops the connection
                _ => Response::result(req.id, Value::Null),
            };
            let _ = ops_tx.send(
                format!("{:?}", req.op)
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .to_string(),
            );
            w.write_all(encode(&reply).as_bytes()).unwrap();
        }
    });
    ops
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tessela-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_for_log(h: &mut Harness, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        seen.extend(h.logs());
        if seen.iter().any(|l| l == want) {
            return;
        }
        if let Ok(i) = h.inbound.recv_timeout(Duration::from_millis(50)) {
            let _ = h.inst.deliver(i);
        }
    }
    panic!("never logged {want:?}; saw {seen:?}");
}

const LUA_BATTERY: &str = r#"
local power = use("power")
local bat = power:subscribe("battery")
return Pill {
  icon = "battery",
  label = function()
    local b = bat()
    if not b then return "—" end
    return b.percent .. "%" .. (b.charging and "+" or "")
  end,
  on_click = function()
    power:call("set_profile", { profile = "performance" }, function(r)
      log("profile is now " .. r.value)
      power:call("hang_up")
    end)
  end,
}
"#;

const TSX_BATTERY: &str = r#"
interface Battery { percent: number; charging: boolean }
export default function BatteryPill() {
  const power = use("power");
  const bat = power.subscribe("battery");
  const label = () => {
    const b = bat() as Battery | null;
    return b ? `${b.percent}%${b.charging ? "+" : ""}` : "—";
  };
  return <Pill icon="battery" label={label}
    onClick={() => power.call("set_profile", { profile: "performance" },
      (r: { value: string }) => { log(`profile is now ${r.value}`); power.call("hang_up"); })} />;
}
"#;

fn battery_flow(tag: &str, file: &str, src: &str) {
    let dir = temp_dir(tag);
    let _ops = power_service(&dir);
    let services: Arc<dyn Module> = Arc::new(UnixServices::new([dir.clone()]));
    let mut h = Harness::new(file, src, &["service:power"], vec![services]);

    let tree = h.pump_until(|n| pill(n).0 == "87%+");
    h.inst.ui(pill(&tree).1.unwrap(), Value::Null).unwrap();
    wait_for_log(&mut h, "profile is now performance");
    // The service hung up: the subscription becomes unavailable (nil/null).
    h.pump_until(|n| pill(n).0 == "—");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lua_service() {
    battery_flow("lua", "battery.luau", LUA_BATTERY);
}

#[test]
fn tsx_service() {
    battery_flow("tsx", "battery.tsx", TSX_BATTERY);
}

#[test]
fn dropping_an_instance_releases_subscriptions() {
    let dir = temp_dir("drop");
    let ops = power_service(&dir);
    let services: Arc<dyn Module> = Arc::new(UnixServices::new([dir.clone()]));
    let mut h = Harness::new(
        "d.luau",
        r#"local b = use("power"):subscribe("battery"); return Text(function() local v = b(); return tostring(v and v.percent) end)"#,
        &["service:power"],
        vec![services],
    );
    h.pump_until(|n| n.str("text") == Some("87"));
    assert_eq!(
        ops.recv_timeout(Duration::from_secs(2)).unwrap(),
        "Subscribe"
    );
    drop(h);
    assert_eq!(
        ops.recv_timeout(Duration::from_secs(2)).unwrap(),
        "Unsubscribe"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- sandbox: capabilities

#[test]
fn lua_has_no_ambient_authority() {
    let mut h = Harness::plain(
        "probe.luau",
        r#"
local probes = {
  io = io, require = require, loadstring = loadstring, getfenv = getfenv,
  dofile = dofile, load = load, execute = os.execute, remove = os.remove,
  getenv = os.getenv, popen = io and io.popen, host = __host, rt = __rt,
  debug_getinfo = debug and debug.getinfo,
}
local leaked = {}
for name, v in pairs(probes) do table.insert(leaked, name) end
table.sort(leaked)
return Text { text = "leaked:" .. table.concat(leaked, ",") }
"#,
    );
    assert_eq!(h.inst.render(), text("leaked:"));
}

#[test]
fn js_has_no_ambient_authority() {
    let mut h = Harness::plain(
        "probe.tsx",
        r#"
const g = globalThis as any;
const probes = ["require", "process", "fetch", "XMLHttpRequest", "setTimeout", "std", "os",
                "Deno", "Bun", "__host", "__rt", "WebSocket", "importScripts"];
export default <Text text={"leaked:" + probes.filter((p) => g[p] !== undefined).join(",")} />;
"#,
    );
    assert_eq!(h.inst.render(), text("leaked:"));
}

#[test]
fn without_the_module_there_is_no_use() {
    // A host that registers no `service` module gives extensions no `use` at all.
    let err = load_err("x.luau", r#"local a = use("power"); return Text "x""#);
    assert!(err.contains("attempt to call a nil value"), "{err}");
    let err = load_err(
        "x.tsx",
        r#"const a = use("power"); export default <Text text="x" />;"#,
    );
    assert!(err.contains("use"), "{err}");
}

#[test]
fn js_cannot_import_anything() {
    let err = load_err(
        "imp.tsx",
        r#"import fs from "fs"; export default <Text text={String(fs)} />;"#,
    );
    assert!(err.contains("fs") || err.contains("module"), "{err}");
    let err = load_err(
        "imp2.tsx",
        r#"const m = await import("./other.js"); export default <Text text="x" />;"#,
    );
    assert!(!err.is_empty());
}

#[test]
fn undeclared_service_is_refused() {
    let services: Arc<dyn Module> = Arc::new(UnixServices::new(Vec::<PathBuf>::new()));
    for (file, src) in [
        ("x.luau", r#"local a = use("audio"); return Text "x""#),
        (
            "x.tsx",
            r#"const a = use("audio"); export default <Text text="x" />;"#,
        ),
    ] {
        let err = Harness::try_new(file, src, &["service:power"], vec![services.clone()])
            .err()
            .unwrap();
        assert!(
            err.contains("did not declare capability 'service:audio'"),
            "{err}"
        );
    }
}

#[test]
fn service_names_cannot_escape_the_directory() {
    let services: Arc<dyn Module> = Arc::new(UnixServices::new(Vec::<PathBuf>::new()));
    let err = Harness::try_new(
        "x.luau",
        r#"use("../../run/user/1000/bus"); return Text "x""#,
        &["service:*"],
        vec![services],
    )
    .err()
    .unwrap();
    assert!(err.contains("invalid service name"), "{err}");
}

#[test]
fn prelude_is_read_only() {
    // Luau sandbox: built-ins and preludes cannot be modified.
    let err = load_err("x.luau", r#"string.rep = nil; return Text "x""#);
    assert!(
        err.to_lowercase().contains("readonly")
            || err.contains("read-only")
            || err.contains("attempt to modify"),
        "{err}"
    );
    // JS globals from the prelude are non-writable, non-configurable.
    let mut h = Harness::plain(
        "x.tsx",
        r#"try { (globalThis as any).signal = () => 1; } catch {} export default <Text text={typeof signal} />;"#,
    );
    assert_eq!(h.inst.render(), text("function"));
}

// ---------------------------------------------------------------- sandbox: resources

fn cpu_bomb(file: &str, src: &str) {
    let start = Instant::now();
    let err = load_err(file, src);
    assert!(err.contains("CPU budget exceeded"), "{err}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "interrupt was too slow: {:?}",
        start.elapsed()
    );
}

#[test]
fn infinite_loops_are_interrupted() {
    cpu_bomb("loop.luau", "while true do end");
    cpu_bomb("loop.tsx", "for (;;) {}");
    // Inside a binding, at render time.
    cpu_bomb(
        "loop2.luau",
        "return Text(function() while true do end end)",
    );
    cpu_bomb(
        "loop2.tsx",
        "export default <Text text={() => { while (true) {} }} />;",
    );
}

#[test]
fn infinite_loop_in_handler_is_interrupted_and_widget_survives() {
    let mut h = Harness::plain(
        "h.luau",
        r#"
local n = signal(0)
return Pill { label = function() return tostring(n()) end,
              on_click = function() n:set(1); while true do end end }
"#,
    );
    let tree = h.inst.render();
    let (_, click) = pill(&tree);
    let err = h.inst.ui(click.unwrap(), Value::Null).unwrap_err();
    assert!(err.0.contains("CPU budget exceeded"));
    assert_eq!(
        pill(&h.inst.render()).0,
        "1",
        "the extension keeps working after the runaway handler"
    );
}

#[test]
fn memory_bombs_are_contained() {
    let err = load_err(
        "mem.luau",
        r#"local t = {} for i = 1, 1e9 do t[i] = string.rep("x", 1024) end return Text "x""#,
    );
    assert!(
        err.contains("memory") || err.contains("CPU budget"),
        "{err}"
    );
    let err = load_err(
        "mem.tsx",
        r#"const a: string[] = []; for (;;) a.push("x".repeat(1024)); export default <Text text="x" />;"#,
    );
    assert!(
        err.contains("memory") || err.contains("CPU budget"),
        "{err}"
    );
}

#[test]
fn huge_trees_are_rejected() {
    let err = load_err(
        "big.tsx",
        r#"export default <Column>{Array.from({ length: 7000 }, (_, i) => <Text text={String(i)} />)}</Column>;"#,
    );
    assert!(err.contains("too large"), "{err}");
}

#[test]
fn unknown_components_are_rejected() {
    let err = load_err(
        "x.luau",
        r#"return { type = "WebView", url = "https://example.com" }"#,
    );
    assert!(err.contains("invalid view tree"), "{err}");
}

#[test]
fn unknown_file_types_name_the_engines() {
    let err = load_err("x.py", "print(1)");
    assert!(
        err.contains("no engine for '.py'") && err.contains("luau") && err.contains("quickjs"),
        "{err}"
    );
}

/// Project files, as the host would serve them (it also refuses symlinks out
/// of the root; see tessela-host).
struct MapSources(std::collections::HashMap<String, String>);

impl tessela::core::Sources for MapSources {
    fn read(&self, path: &str) -> Option<String> {
        self.0.get(path).cloned()
    }
}

fn project(entry: &str, files: &[(&str, &str)]) -> Result<Node, String> {
    let sources: std::collections::HashMap<String, String> = files
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let source = sources[entry].clone();
    let mut inst = Instance::new(InstanceSpec {
        name: "project",
        file: entry,
        source: &source,
        grants: Grants::new([]),
        limits: limits(),
        engines: &tessela::engines(),
        modules: vec![],
        vocabulary: Arc::new(Vocabulary::default()),
        sink: Sink::none(),
        persist: None,
        sources: Some(Arc::new(MapSources(sources))),
        trace: None,
    })
    .map_err(|e| e.0)?;
    Ok(inst.render())
}

#[test]
fn js_imports_resolve_inside_the_project_only() {
    let files = [
        (
            "src/main.tsx",
            r#"import { Tile } from "../lib/tile"; import { n } from "./n"; export default <Tile label={`x${n}`} />;"#,
        ),
        ("src/n.ts", "export const n: number = 2;"),
        (
            "lib/tile.tsx",
            r#"export function Tile(p: { label: string }) { return <Text text={p.label} />; }"#,
        ),
        (
            "escape.tsx",
            r#"import x from "../outside"; export default <Text text={String(x)} />;"#,
        ),
        (
            "bare.tsx",
            r#"import x from "fs"; export default <Text text={String(x)} />;"#,
        ),
        (
            "missing.tsx",
            r#"import x from "./nope"; export default <Text text={String(x)} />;"#,
        ),
    ];
    let node = project("src/main.tsx", &files).unwrap();
    assert_eq!(node.str("text"), Some("x2"));
    for (entry, needle) in [
        ("escape.tsx", "outside the project"),
        ("bare.tsx", "only relative imports"),
        ("missing.tsx", "cannot find"),
    ] {
        let err = project(entry, &files).unwrap_err();
        assert!(err.contains(needle), "{entry}: {err}");
    }
}

#[test]
fn jsx_survives_a_variable_named_h() {
    let files = [(
        "main.tsx",
        r#"const hours = ["22h", "23h"];
           export default <Row>{hours.map((h) => <Text key={h} text={h} />)}</Row>;"#,
    )];
    let node = project("main.tsx", &files).unwrap();
    assert_eq!(node.children.len(), 2, "{node:?}");
    assert_eq!(node.children[1].str("text"), Some("23h"));
}
