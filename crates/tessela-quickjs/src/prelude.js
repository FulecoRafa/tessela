// Core prelude: signals, log, JSX factory and component names. Runs before
// module preludes; every global defined here is frozen, so an extension
// cannot replace `signal` or `h` for code that runs after it. `__host` and
// `__rt` are deleted before extension code runs.
"use strict";
(() => {
  const host = globalThis.__host;
  const enc = (v) => (v === undefined ? "null" : JSON.stringify(v));
  const dec = (s) => JSON.parse(s);
  const signals = new WeakSet();

  const wrap = (id) => {
    const s = () => dec(host.get(id));
    s.peek = () => dec(host.peek(id));
    s.set = (v) => host.set(id, enc(v));
    s.update = (f) => host.set(id, enc(f(dec(host.peek(id)))));
    signals.add(s);
    return Object.freeze(s);
  };

  const signal = (initial) => wrap(host.sigNew(enc(initial)));
  const log = (...parts) => host.log(parts.map(String).join(" "));

  const Fragment = "Fragment";
  // JSX factory. Function components run once (Solid-style); reactivity comes
  // from function-valued props, e.g. label={() => `${vol().volume}%`} or label={vol}.
  const h = (type, props, ...children) => {
    props = props ?? {};
    if (typeof type === "function") return type({ ...props, children });
    if (type === Fragment) return children;
    const out = { ...props, type };
    if (children.length) out.children = children;
    return out;
  };

  const globals = { signal, log, h, Fragment };
  for (const c of dec(host.components)) globals[c.name] = c.name;
  for (const [k, v] of Object.entries(globals)) {
    Object.defineProperty(globalThis, k, { value: v, writable: false, configurable: false, enumerable: false });
  }

  // Private helpers for module preludes.
  globalThis.__rt = Object.freeze({
    invoke: (module, func, args) => dec(host.invoke(module, func, args ?? null)),
    wrap,
    isSignal: (v) => signals.has(v),
    define: (name, value) =>
      Object.defineProperty(globalThis, name, { value, writable: false, configurable: false, enumerable: false }),
  });
})();
