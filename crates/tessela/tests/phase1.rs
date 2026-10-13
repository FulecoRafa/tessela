//! Phase 1 features: declarative actions, timers, persist, quotas, keys,
//! app components with fallbacks, patches and the worker pool.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use serde_json::{Value, json};
use tessela::core::view::{ComponentSpec, Fallback, PropSpec, PropType};
use tessela::core::{
    Grants, Handler, Inbound, Instance, InstanceSpec, Limits, Module, Node, PersistStore, Sink,
    Vocabulary,
};
use tessela::host::{App, Trees};
use tessela::proto::{Op, Request, Response, encode};
use tessela::timer::Timers;
use tessela::unix::UnixServices;
use tessela::view::ERROR;

struct H {
    inst: Instance,
    inbound: Receiver<Inbound>,
}

fn build(
    file: &str,
    src: &str,
    caps: &[&str],
    modules: Vec<Arc<dyn Module>>,
    limits: Limits,
    persist: Option<PersistStore>,
) -> Result<H, String> {
    let (tx, inbound) = unbounded();
    let inst = Instance::new(InstanceSpec {
        name: "t",
        file,
        source: src,
        grants: Grants::new(caps.iter().map(|s| s.to_string())),
        limits,
        engines: &tessela::engines(),
        modules,
        vocabulary: Arc::new(Vocabulary::default()),
        sink: Sink::new(move |i| {
            let _ = tx.send(i);
        }),
        persist,
        sources: None,
    })
    .map_err(|e| e.0)?;
    Ok(H { inst, inbound })
}

fn h(file: &str, src: &str, caps: &[&str], modules: Vec<Arc<dyn Module>>) -> H {
    build(file, src, caps, modules, Limits::default(), None)
        .unwrap_or_else(|e| panic!("load failed: {e}"))
}

impl H {
    /// Deliver module inputs for `d`, rendering after each.
    fn pump(&mut self, d: Duration) -> Node {
        let until = Instant::now() + d;
        while let Ok(i) = self.inbound.recv_deadline(until) {
            if let Err(e) = self.inst.deliver(i) {
                panic!("delivery failed: {e}");
            }
        }
        self.inst.render()
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tessela-p1-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A `power` service that records every call (method + args).
fn recording_service(dir: &Path) -> Receiver<(String, Value)> {
    let listener = UnixListener::bind(dir.join("power.sock")).unwrap();
    let (tx, rx) = unbounded();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let tx = tx.clone();
            thread::spawn(move || {
                let mut w = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { return };
                    let req: Request = serde_json::from_str(&line).unwrap();
                    let reply = match req.op {
                        Op::Subscribe { .. } => Response::event(req.id, json!(1)),
                        Op::Call { method, args } => {
                            let _ = tx.send((method, args.clone()));
                            Response::result(req.id, Value::Null)
                        }
                        _ => Response::result(req.id, Value::Null),
                    };
                    let _ = w.write_all(encode(&reply).as_bytes());
                }
            });
        }
    });
    rx
}

// ---------------------------------------------------------------- declarative actions

const ACTION_LUAU: &str = r#"
local power = use("power")
return Pill {
  label = "p",
  on_click = power:action("set_profile", { profile = "performance" }),
  on_scroll = power:action("step"),
}
"#;

const ACTION_TSX: &str = r#"
const power = use("power");
export default <Pill label="p"
  onClick={power.action("set_profile", { profile: "performance" })}
  onScroll={power.action("step")} />;
"#;

fn actions_reach_the_module_without_guest_code(tag: &str, file: &str, src: &str) {
    let dir = temp_dir(tag);
    let calls = recording_service(&dir);
    let svc: Arc<dyn Module> = Arc::new(UnixServices::new([dir.clone()]));
    let mut h = h(file, src, &["service:power"], vec![svc]);
    let node = h.inst.render();
    let click = node.handler("on_click").unwrap();
    assert!(matches!(click, Handler::Action { .. }), "{click:?}");
    let (bindings, _) = h.inst.live_counts();

    h.inst.trigger(&click, Value::Null).unwrap();
    let (method, args) = calls.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        (method.as_str(), args),
        ("set_profile", json!({ "profile": "performance" }))
    );

    // Event data rides along as `event`.
    h.inst
        .trigger(&node.handler("on_scroll").unwrap(), json!({ "delta": -1 }))
        .unwrap();
    let (method, args) = calls.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        (method.as_str(), args),
        ("step", json!({ "event": { "delta": -1 } }))
    );

    assert_eq!(
        h.inst.live_counts().0,
        bindings,
        "no guest function was registered or run"
    );
    assert!(!h.inst.needs_render());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn luau_actions() {
    actions_reach_the_module_without_guest_code("act-lua", "a.luau", ACTION_LUAU);
}

#[test]
fn tsx_actions() {
    actions_reach_the_module_without_guest_code("act-tsx", "a.tsx", ACTION_TSX);
}

#[test]
fn forged_actions_still_need_the_capability() {
    let svc: Arc<dyn Module> = Arc::new(UnixServices::new(Vec::<PathBuf>::new()));
    let mut h = h(
        "f.luau",
        r#"return Button { label = "x", on_click = { ["$action"] = { module = "service", func = "call",
             args = { service = "audio", method = "mute" } } } }"#,
        &["service:power"],
        vec![svc],
    );
    let click = h.inst.render().handler("on_click").unwrap();
    let err = h.inst.trigger(&click, Value::Null).unwrap_err();
    assert!(
        err.0.contains("did not declare capability 'service:audio'"),
        "{err}"
    );
}

// ---------------------------------------------------------------- timers

fn timers() -> Vec<Arc<dyn Module>> {
    vec![Arc::new(Timers::new())]
}

#[test]
fn after_fires_once_and_frees_its_quota() {
    for (file, src) in [
        (
            "t.luau",
            r#"local n = signal(0); after(30, function() n:update(function(v) return v + 1 end) end)
           return Text(function() return tostring(n()) end)"#,
        ),
        (
            "t.tsx",
            r#"const n = signal(0); after(30, () => n.update((v: number) => v + 1));
           export default <Text text={() => String(n())} />;"#,
        ),
    ] {
        let mut h = h(file, src, &["timer"], timers());
        assert_eq!(h.inst.live_resources(), 1);
        let n = h.pump(Duration::from_millis(250));
        assert_eq!(n.str("text"), Some("1"), "{file}");
        assert_eq!(
            h.inst.live_resources(),
            0,
            "{file}: a fired one-shot no longer counts"
        );
    }
}

#[test]
fn every_repeats_and_cancel_stops_it() {
    let mut h = h(
        "e.luau",
        r#"
local n = signal(0)
local stop
stop = every(20, function()
  n:update(function(v) return v + 1 end)
  if n:peek() >= 3 then stop() end
end)
return Text(function() return tostring(n()) end)
"#,
        &["timer"],
        timers(),
    );
    let n = h.pump(Duration::from_millis(300));
    assert_eq!(n.str("text"), Some("3"));
    assert_eq!(h.inst.live_resources(), 0);
}

#[test]
fn timers_belong_to_their_binding() {
    // Each time `mode` changes the binding re-runs: the previous `every` must stop.
    let mut h = h(
        "b.luau",
        r#"
local mode = signal("a")
local ticks = signal(0)
return Pill {
  label = function()
    local m = mode()
    every(20, function() ticks:update(function(v) return v + 1 end) end)
    return m
  end,
  on_click = function() mode:set(mode:peek() == "a" and "b" or "a") end,
}
"#,
        &["timer"],
        timers(),
    );
    let node = h.inst.render();
    let click = node.handler("on_click").unwrap();
    for _ in 0..5 {
        h.inst.trigger(&click, Value::Null).unwrap();
        h.inst.render();
    }
    assert_eq!(
        h.inst.live_resources(),
        1,
        "only the latest binding run owns a timer"
    );
    drop(h);
}

#[test]
fn timers_need_the_capability() {
    let err = build(
        "t.luau",
        "after(10, function() end); return Text 'x'",
        &[],
        timers(),
        Limits::default(),
        None,
    )
    .err()
    .unwrap();
    assert!(err.contains("did not declare capability 'timer'"), "{err}");
}

// ---------------------------------------------------------------- persist

#[test]
fn persisted_signals_survive_a_reload() {
    let store = PersistStore::default();
    for (file, src) in [
        (
            "p.luau",
            r#"local on = persist("on", false)
           return Pill { label = function() return tostring(on()) end, on_click = function() on:set(true) end }"#,
        ),
        (
            "p.tsx",
            r#"const on = persist("on2", false);
           export default <Pill label={() => String(on())} onClick={() => on.set(true)} />;"#,
        ),
    ] {
        let mut a = build(
            file,
            src,
            &[],
            vec![],
            Limits::default(),
            Some(store.clone()),
        )
        .unwrap();
        let click = a.inst.render().handler("on_click").unwrap();
        a.inst.trigger(&click, Value::Null).unwrap();
        assert_eq!(a.inst.render().str("label"), Some("true"));
        drop(a);
        let mut b = build(
            file,
            src,
            &[],
            vec![],
            Limits::default(),
            Some(store.clone()),
        )
        .unwrap();
        assert_eq!(b.inst.render().str("label"), Some("true"), "{file}");
    }
}

// ---------------------------------------------------------------- quotas

#[test]
fn call_rate_is_limited() {
    let limits = Limits {
        calls_per_sec: 20,
        ..Limits::default()
    };
    let err = build(
        "q.luau",
        "for i = 1, 100 do after(1000, function() end) end return Text 'x'",
        &["timer"],
        timers(),
        limits,
        None,
    )
    .err()
    .unwrap();
    assert!(err.contains("exceeded 20 module calls per second"), "{err}");
}

#[test]
fn live_resources_are_limited() {
    let limits = Limits {
        max_resources: 10,
        ..Limits::default()
    };
    let err = build(
        "q.tsx",
        "for (let i = 0; i < 11; i++) after(60000, () => {}); export default <Text text='x' />;",
        &["timer"],
        timers(),
        limits,
        None,
    )
    .err()
    .unwrap();
    assert!(err.contains("too many resources (limit 10)"), "{err}");
}

// ---------------------------------------------------------------- keys

#[test]
fn guests_can_key_nodes() {
    let mut a = h(
        "k.luau",
        r#"return Row { Text { key = "a", text = "x" }, Text { key = 2, text = "y" } }"#,
        &[],
        vec![],
    );
    let n = a.inst.render();
    assert_eq!(n.children[0].key.as_deref(), Some("a"));
    assert_eq!(n.children[1].key.as_deref(), Some("2"));
    let mut b = h(
        "k.tsx",
        r#"export default <Row>{["a", "b"].map((k) => <Text key={k} text={k} />)}</Row>;"#,
        &[],
        vec![],
    );
    assert_eq!(b.inst.render().children[1].key.as_deref(), Some("b"));
}

// ---------------------------------------------------------------- app: pool, patches, components

fn write_ext(root: &Path, name: &str, file: &str, src: &str, caps: &str) {
    let d = root.join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("widget.toml"),
        format!("name = \"{name}\"\nentry = \"{file}\"\ncapabilities = [{caps}]\n"),
    )
    .unwrap();
    std::fs::write(d.join(file), src).unwrap();
}

fn meter() -> ComponentSpec {
    const PROPS: &[PropSpec] = &[
        PropSpec {
            name: "label",
            ty: PropType::String,
            required: true,
        },
        PropSpec {
            name: "value",
            ty: PropType::Number,
            required: true,
        },
    ];
    let fallback: Fallback = Arc::new(|n: &Node| {
        Node::new("Row")
            .prop("gap", 4)
            .child(Node::new("Text").prop("text", n.str("label").unwrap_or("")))
            .child(Node::new("Progress").prop("value", n.f32("value").unwrap_or(0.0)))
    });
    ComponentSpec {
        name: "Meter",
        shorthand: None,
        props: PROPS,
        children: false,
        fallback: Some(fallback),
    }
}

fn wait_for(
    surface: &tessela::host::Surface,
    trees: &mut Trees,
    mut done: impl FnMut(&Trees) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(trees) {
        match surface.frames.recv_deadline(deadline) {
            Ok(f) => trees.apply(&f),
            Err(_) => panic!("timed out: {:?}", trees.nodes()),
        }
    }
}

#[test]
fn many_extensions_share_a_small_pool() {
    let root = temp_dir("pool");
    for i in 0..12 {
        let (file, src) = if i % 2 == 0 {
            (
                "w.luau",
                format!("return Meter {{ label = \"m{i}\", value = 0.5 }}"),
            )
        } else {
            (
                "w.tsx",
                format!("export default <Meter label=\"m{i}\" value={{0.5}} />;"),
            )
        };
        write_ext(&root, &format!("e{i}"), file, &src, "");
    }
    let app = (0..12)
        .fold(
            App::builder()
                .engines(tessela::engines())
                .workers(2)
                .component(meter())
                .unwrap(),
            |b, i| b.extension(root.join(format!("e{i}"))),
        )
        .build()
        .unwrap();
    let surface = app.start();
    let mut trees = Trees::new(12);
    wait_for(&surface, &mut trees, |t| {
        t.nodes().iter().all(Option::is_some)
    });
    for (i, n) in trees.nodes().iter().enumerate() {
        let n = n.as_ref().unwrap();
        // The renderer only knows core components: Meter arrives lowered.
        assert!(n.is("Row"), "slot {i}: {n:?}");
        assert_eq!(n.children[0].str("text"), Some(format!("m{i}").as_str()));
    }
    assert_eq!(surface.workers, 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hot_reload_keeps_ids_and_persisted_state() {
    let root = temp_dir("reload");
    let src = r#"local on = persist("on", false)
return Row { Text "title", Pill { label = function() return on() and "on" or "off" end,
  on_click = function() on:set(true) end } }"#;
    write_ext(&root, "w", "w.luau", src, "");
    let surface = App::builder()
        .engines(tessela::engines())
        .extension(root.join("w"))
        .build()
        .unwrap()
        .start();
    let mut trees = Trees::new(1);
    wait_for(&surface, &mut trees, |t| t.nodes()[0].is_some());
    let title_id = trees
        .tree(0)
        .unwrap()
        .get(trees.tree(0).unwrap().root().unwrap())
        .unwrap()
        .children[0];

    let pill = trees.nodes()[0].as_ref().unwrap().children[1].clone();
    surface.send(
        0,
        tessela::host::UiEvent::new(pill.handler("on_click").unwrap(), Value::Null),
    );
    wait_for(&surface, &mut trees, |t| {
        t.nodes()[0].as_ref().unwrap().children[1].str("label") == Some("on")
    });

    // Edit the file: the extension reloads, keeps `on`, and the untouched
    // title keeps its node id (only changed nodes are patched).
    thread::sleep(Duration::from_millis(1100)); // coarse mtime on some filesystems
    std::fs::write(
        root.join("w/w.luau"),
        src.replace(r#""on" or "off""#, r#""ON" or "off""#),
    )
    .unwrap();
    wait_for(&surface, &mut trees, |t| {
        t.nodes()[0].as_ref().unwrap().children[1].str("label") == Some("ON")
    });
    let tree = trees.tree(0).unwrap();
    assert_eq!(
        tree.get(tree.root().unwrap()).unwrap().children[0],
        title_id
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn broken_extensions_render_an_error_node() {
    let root = temp_dir("broken");
    write_ext(&root, "bad", "b.luau", "return Text { text = 1 }", "");
    let surface = App::builder()
        .engines(tessela::engines())
        .extension(root.join("bad"))
        .build()
        .unwrap()
        .start();
    let mut trees = Trees::new(1);
    wait_for(&surface, &mut trees, |t| t.nodes()[0].is_some());
    let n = trees.nodes()[0].as_ref().unwrap();
    assert!(n.is(ERROR));
    assert!(
        n.str("message")
            .unwrap()
            .contains("Text.text must be a string"),
        "{n:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
