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
  // Like `signal`, but the value survives reloads of this extension.
  const persist = (key, initial) => wrap(host.persist(String(key), enc(initial)));
  // The stack tells devtools where the call came from.
  const log = (...parts) => host.log(parts.map(String).join(" "), new Error().stack);
  // A derived value: `f` re-runs when what it reads changes, and code that
  // reads the memo re-runs only when the result differs (Solid's createMemo).
  const memo = (f) => {
    const s = wrap(host.memo(f));
    const read = () => s();
    read.peek = s.peek;
    signals.add(read);
    return Object.freeze(read);
  };

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

  // `__jsx` is what transpiled JSX calls; `h` is the same, for hand-written trees.
  const globals = { signal, persist, memo, log, h, Fragment, __jsx: h, __jsxFragment: Fragment };
  for (const c of dec(host.components)) globals[c.name] = c.name;
  for (const [k, v] of Object.entries(globals)) {
    Object.defineProperty(globalThis, k, { value: v, writable: false, configurable: false, enumerable: false });
  }

  // Private helpers for module preludes.
  globalThis.__rt = Object.freeze({
    invoke: (module, func, args) => dec(host.invoke(module, func, args ?? null)),
    wrap,
    isSignal: (v) => signals.has(v),
    // A declarative action: the host calls the module when the event fires,
    // without running any extension code.
    action: (module, func, args) => Object.freeze({ $action: { module, func, args: args ?? null } }),
    define: (name, value) =>
      Object.defineProperty(globalThis, name, { value, writable: false, configurable: false, enumerable: false }),
  });
})();
