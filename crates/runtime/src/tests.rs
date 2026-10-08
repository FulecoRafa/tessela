use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use view::Tone;

fn limits() -> Limits {
    Limits {
        memory_bytes: 8 * 1024 * 1024,
        budget: Duration::from_millis(100),
        max_values: 20_000,
    }
}

fn widget(file: &str, src: &str, uses: &[&str]) -> Widget {
    Widget::new(
        "test",
        file,
        src,
        uses.iter().map(|s| s.to_string()).collect(),
        limits(),
    )
    .unwrap_or_else(|e| panic!("load failed: {e}"))
}

fn load_err(file: &str, src: &str) -> String {
    match Widget::new("test", file, src, vec![], limits()) {
        Ok(mut w) => match w.render() {
            Node::Error { message, .. } => message,
            other => panic!("expected an error, rendered {other:?}"),
        },
        Err(e) => e.0,
    }
}

fn pill(node: &Node) -> (&str, Option<FnId>) {
    match node {
        Node::Pill {
            label, on_click, ..
        } => (label, *on_click),
        Node::Row { children, .. } => pill(&children[0]),
        other => panic!("not a pill: {other:?}"),
    }
}

fn logs(w: &mut Widget) -> Vec<String> {
    w.take_effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::Log { message, .. } => Some(message),
            _ => None,
        })
        .collect()
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
    let mut w = widget(file, src, &[]);
    let tree = w.render();
    let (label, click) = pill(&tree);
    assert_eq!(label, "clicks: 0");
    let Node::Row { children, gap } = &tree else {
        panic!()
    };
    assert_eq!(*gap, 4.0);
    assert_eq!(
        children[1],
        Node::Text {
            text: "static".into(),
            tone: Tone::Muted
        }
    );
    assert_eq!(logs(&mut w), ["label"]);

    // Nothing changed: re-render must not re-enter the guest.
    w.render();
    assert!(logs(&mut w).is_empty(), "clean bindings are cached");

    w.input(Input::Ui {
        handler: click.unwrap(),
        args: json!(null),
    })
    .unwrap();
    assert!(w.needs_render());
    let tree = w.render();
    assert_eq!(pill(&tree).0, "clicks: 1");
    assert_eq!(logs(&mut w), ["label"], "only the dependent binding re-ran");
}

#[test]
fn lua_counter() {
    counter_behaves("counter.luau", LUA_COUNTER);
}

#[test]
fn tsx_counter() {
    counter_behaves("counter.tsx", TSX_COUNTER);
}

fn service_flow(file: &str, src: &str) {
    let mut w = widget(file, src, &["power"]);
    let effects = w.take_effects();
    let token = match &effects[..] {
        [
            Effect::Subscribe {
                service,
                path,
                token,
            },
        ] => {
            assert_eq!((service.as_str(), path.as_str()), ("power", "battery"));
            *token
        }
        other => panic!("expected one subscribe, got {other:?}"),
    };
    assert_eq!(pill(&w.render()).0, "—", "no data yet");

    w.input(Input::Event {
        token,
        value: json!({"percent": 87, "charging": true}),
    })
    .unwrap();
    let tree = w.render();
    let (label, click) = pill(&tree);
    assert_eq!(label, "87%+");

    w.input(Input::Ui {
        handler: click.unwrap(),
        args: json!(null),
    })
    .unwrap();
    let call = w.take_effects().into_iter().find_map(|e| match e {
        Effect::Call {
            service,
            method,
            args,
            token,
        } => Some((service, method, args, token)),
        _ => None,
    });
    let (service, method, args, call_token) = call.expect("click issues a call");
    assert_eq!(
        (service.as_str(), method.as_str()),
        ("power", "set_profile")
    );
    assert_eq!(args, json!({"profile": "performance"}));

    w.input(Input::CallResult {
        token: call_token,
        result: Ok(json!("performance")),
    })
    .unwrap();
    assert_eq!(logs(&mut w), ["profile is now performance"]);

    w.input(Input::Unavailable { token }).unwrap();
    assert_eq!(pill(&w.render()).0, "—", "outage resets the signal");
}

#[test]
fn lua_service() {
    service_flow(
        "battery.luau",
        r#"
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
    end)
  end,
}
"#,
    );
}

#[test]
fn tsx_service() {
    service_flow(
        "battery.tsx",
        r#"
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
      (r: { value: string }) => log(`profile is now ${r.value}`))} />;
}
"#,
    );
}

#[test]
fn whole_widget_function_rerenders_react_style() {
    let mut w = widget(
        "w.luau",
        r#"
local on = signal(false)
return function()
  return Pill { label = on() and "on" or "off", active = on(), on_click = function() on:set(not on:peek()) end }
end
"#,
        &[],
    );
    let t = w.render();
    let (label, click) = pill(&t);
    assert_eq!(label, "off");
    w.input(Input::Ui {
        handler: click.unwrap(),
        args: json!(null),
    })
    .unwrap();
    let t = w.render();
    assert!(matches!(t, Node::Pill { active: true, .. }));
    // The old click handler was owned by the previous render and is gone.
    assert!(
        w.input(Input::Ui {
            handler: click.unwrap(),
            args: json!(null)
        })
        .is_err()
    );
}

#[test]
fn rebuilding_lists_does_not_leak() {
    let mut w = widget(
        "list.tsx",
        r#"
const items = signal<number[]>([1, 2, 3]);
export default () => (
  <Column>
    {() => items().map((i: number) => <Button label={() => `#${i}`} onClick={() => items.set([...items.peek(), i])} />)}
  </Column>
);
"#,
        &[],
    );
    w.render();
    assert_eq!(w.live_counts(), (3 + 1, 1));
    for _ in 0..50 {
        let handler = first_button(&w.render());
        w.input(Input::Ui {
            handler,
            args: json!(null),
        })
        .unwrap();
        w.render();
    }
    let Node::Column { children, .. } = w.render() else {
        panic!()
    };
    assert_eq!(children.len(), 53);
    let (bindings, signals) = w.live_counts();
    // One label binding per item plus the list binding; nothing from older renders.
    assert_eq!(bindings, 53 + 1);
    assert_eq!(signals, 1);
}

fn first_button(n: &Node) -> FnId {
    match n {
        Node::Column { children, .. } => first_button(&children[0]),
        Node::Button { on_click, .. } => on_click.unwrap(),
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------- sandbox: capabilities

#[test]
fn lua_has_no_ambient_authority() {
    let mut w = widget(
        "probe.luau",
        r#"
local probes = {
  io = io, require = require, loadstring = loadstring, getfenv = getfenv,
  dofile = dofile, load = load, execute = os.execute, remove = os.remove,
  getenv = os.getenv, popen = io and io.popen, host = __host, debug_getinfo = debug and debug.getinfo,
}
local leaked = {}
for name, v in pairs(probes) do table.insert(leaked, name) end
table.sort(leaked)
return Text { text = "leaked:" .. table.concat(leaked, ",") }
"#,
        &[],
    );
    assert_eq!(
        w.render(),
        Node::Text {
            text: "leaked:".into(),
            tone: Tone::Normal
        }
    );
}

#[test]
fn js_has_no_ambient_authority() {
    let mut w = widget(
        "probe.tsx",
        r#"
const g = globalThis as any;
const probes = ["require", "process", "fetch", "XMLHttpRequest", "setTimeout", "std", "os",
                "Deno", "Bun", "__host", "WebSocket", "importScripts"];
export default <Text text={"leaked:" + probes.filter((p) => g[p] !== undefined).join(",")} />;
"#,
        &[],
    );
    assert_eq!(
        w.render(),
        Node::Text {
            text: "leaked:".into(),
            tone: Tone::Normal
        }
    );
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
    let err = load_err("x.luau", r#"local a = use("audio"); return Text "x""#);
    assert!(err.contains("did not declare service 'audio'"), "{err}");
    let err = load_err(
        "x.tsx",
        r#"const a = use("audio"); export default <Text text="x" />;"#,
    );
    assert!(err.contains("did not declare service 'audio'"), "{err}");
}

#[test]
fn prelude_is_read_only() {
    // Luau sandbox: built-ins and prelude globals cannot be modified.
    let err = load_err("x.luau", r#"string.rep = nil; return Text "x""#);
    assert!(
        err.to_lowercase().contains("readonly")
            || err.contains("read-only")
            || err.contains("attempt to modify"),
        "{err}"
    );
    // A widget may shadow `use` in its own environment, but that never affects the host side.
    let mut w = widget(
        "x.tsx",
        r#"try { (globalThis as any).use = () => 1; } catch {} export default <Text text={typeof use} />;"#,
        &[],
    );
    assert_eq!(
        w.render(),
        Node::Text {
            text: "function".into(),
            tone: Tone::Normal
        }
    );
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
    let mut w = widget(
        "h.luau",
        r#"
local n = signal(0)
return Pill { label = function() return tostring(n()) end,
              on_click = function() n:set(1); while true do end end }
"#,
        &[],
    );
    let (_, click) = pill(&w.render());
    let err = w
        .input(Input::Ui {
            handler: click.unwrap(),
            args: json!(null),
        })
        .unwrap_err();
    assert!(err.0.contains("CPU budget exceeded"));
    assert_eq!(
        pill(&w.render()).0,
        "1",
        "the widget keeps working after the runaway handler"
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
