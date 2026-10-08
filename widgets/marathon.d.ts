// Editor types for TSX widgets. At runtime these are frozen globals provided
// by the shell's prelude; there is nothing to import.

/** A reactive value. Call it to read (tracked inside function props). */
interface Signal<T> {
  (): T;
  peek(): T;
}
interface WritableSignal<T> extends Signal<T> {
  set(value: T): void;
  update(fn: (prev: T) => T): void;
}

type CallResult<T = unknown> = { ok: true; value: T } | { ok: false; error: string };

interface ServiceHandle {
  /** Current value first, then every change. `null` while the service is down. */
  subscribe<T = unknown>(path: string): Signal<T | null>;
  call<T = unknown>(method: string, args?: unknown, callback?: (r: CallResult<T>) => void): void;
}

declare function signal<T>(initial: T): WritableSignal<T>;
/** Throws unless `name` is listed in this widget's `uses` (widget.toml). */
declare function use(name: string): ServiceHandle;
declare function log(...parts: unknown[]): void;

/** A prop that is either a value or a function returning it (a binding). */
type Bind<T> = T | (() => T);
type Tone = "normal" | "muted" | "accent" | "warning" | "critical";
type Child = ViewNode | string | number | null | false | undefined | (() => Child | Child[]) | Child[];

interface ViewNode {
  readonly type: string;
}

declare function Row(props: { gap?: Bind<number>; children?: Child }): ViewNode;
declare function Column(props: { gap?: Bind<number>; children?: Child }): ViewNode;
declare function Text(props: { text: Bind<string>; tone?: Bind<Tone> }): ViewNode;
declare function Icon(props: { name: Bind<string> }): ViewNode;
declare function Pill(props: {
  icon?: Bind<string>;
  label: Bind<string>;
  active?: Bind<boolean>;
  onClick?: () => void;
  onScroll?: (e: { delta: number }) => void;
}): ViewNode;
declare function Button(props: { label: Bind<string>; onClick?: () => void }): ViewNode;
declare function Progress(props: { value: Bind<number>; label?: Bind<string> }): ViewNode;
declare function Spacer(props: {}): ViewNode;

declare function h(type: unknown, props: unknown, ...children: unknown[]): ViewNode;
declare const Fragment: unique symbol;

declare namespace JSX {
  type Element = ViewNode;
  interface ElementChildrenAttribute {
    children: {};
  }
}
