// Runs once per widget, before the widget module. `__host` is captured here and
// deleted from the global object; every global defined below is frozen, so a
// widget cannot replace `use` or `signal` for code that runs after it.
"use strict";
(() => {
  const host = globalThis.__host;
  delete globalThis.__host;
  const enc = (v) => (v === undefined ? "null" : JSON.stringify(v));
  const dec = (s) => JSON.parse(s);

  const wrap = (id) => {
    const s = () => dec(host.get(id));
    s.peek = () => dec(host.peek(id));
    s.set = (v) => host.set(id, enc(v));
    s.update = (f) => host.set(id, enc(f(dec(host.peek(id)))));
    return Object.freeze(s);
  };

  const signal = (initial) => wrap(host.sigNew(enc(initial)));

  // `use("audio")` throws unless the widget manifest lists "audio".
  const use = (name) => {
    host.use(name);
    return Object.freeze({
      subscribe: (path) => wrap(host.subscribe(name, path)),
      call: (method, args, cb) => host.call(name, method, enc(args ?? null), cb ?? null),
    });
  };

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

  const names = ["Row", "Column", "Text", "Icon", "Pill", "Button", "Progress", "Spacer"];
  const globals = { signal, use, log, h, Fragment };
  for (const n of names) globals[n] = n;
  for (const [k, v] of Object.entries(globals)) {
    Object.defineProperty(globalThis, k, { value: v, writable: false, configurable: false, enumerable: false });
  }
})();
