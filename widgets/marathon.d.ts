// Editor types for TSX extensions. At runtime these are frozen globals provided
// by the preludes (core, `service` module, `timer` module); nothing to import.

/** A reactive value. Call it to read (tracked inside function props). */
interface Signal<T> {
  (): T;
  peek(): T;
}
interface WritableSignal<T> extends Signal<T> {
  set(value: T): void;
  update(fn: (prev: T) => T): void;
}

declare function signal<T>(initial: T): WritableSignal<T>;
/** Like `signal`, but the value survives hot reloads of this extension. */
declare function persist<T>(key: string, initial: T): WritableSignal<T>;
declare function log(...parts: unknown[]): void;

/** A declarative event handler: the host calls a module, no extension code runs. */
interface Action {
  readonly $action: { module: string; func: string; args: unknown };
}

// ---- module `service` (capability "service:<name>")

type CallResult<T = unknown> = { ok: true; value: T } | { ok: false; error: string };

interface ServiceHandle {
  /** Current value first, then every change. `null` while the service is down. */
  subscribe<T = unknown>(path: string): Signal<T | null>;
  call<T = unknown>(method: string, args?: unknown, callback?: (r: CallResult<T>) => void): void;
  /** For event props; event data (e.g. scroll delta) arrives in the call args as `event`. */
  action(method: string, args?: Record<string, unknown>): Action;
}

/** Throws unless the manifest lists `service:<name>`. */
declare function use(name: string): ServiceHandle;

// ---- module `timer` (capability "timer"); timers stop with the code that started them

/** Returns a function that cancels the timer. */
declare function after(ms: number, fn: () => void): () => void;
/** At least 16 ms apart. Returns a function that cancels the timer. */
declare function every(ms: number, fn: () => void): () => void;

// ---- components

/** A prop that is either a value or a function returning it (a binding). */
type Bind<T> = T | (() => T);
type Tone = "normal" | "muted" | "accent" | "warning" | "critical";
type Child = ViewNode | string | number | null | false | undefined | (() => Child | Child[]) | Child[];
type On<P = void> = ((payload: P) => void) | Action;

interface ViewNode {
  readonly type: string;
}

/** Every component accepts a `key`, so lists keep node identity when reordered. */
interface Keyed {
  key?: string | number;
}

interface Events {
  onClick?: On;
  onPress?: On;
  onRelease?: On;
  onScroll?: On<{ delta: 1 | -1 }>;
}

declare function Row(props: Keyed & { gap?: Bind<number>; children?: Child }): ViewNode;
declare function Column(props: Keyed & { gap?: Bind<number>; children?: Child }): ViewNode;
declare function Text(props: Keyed & { text: Bind<string>; tone?: Bind<Tone> }): ViewNode;
declare function Icon(props: Keyed & { name: Bind<string>; tone?: Bind<Tone> }): ViewNode;
declare function Pill(props: Keyed & Events & { icon?: Bind<string>; label: Bind<string>; active?: Bind<boolean> }): ViewNode;
declare function Button(props: Keyed & Events & { label: Bind<string> }): ViewNode;
declare function Progress(props: Keyed & { value: Bind<number>; label?: Bind<string> }): ViewNode;
declare function Spacer(props: Keyed): ViewNode;

/** marathon's own component (falls back to Icon + Progress). */
declare function Meter(props: Keyed & { icon?: Bind<string>; label?: Bind<string>; value: Bind<number> }): ViewNode;

declare function h(type: unknown, props: unknown, ...children: unknown[]): ViewNode;
declare const Fragment: unique symbol;

declare namespace JSX {
  type Element = ViewNode;
  interface IntrinsicAttributes extends Keyed {}
  interface ElementChildrenAttribute {
    children: {};
  }
}
