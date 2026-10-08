-- Editor types for Luau extensions (luau-lsp: --definitions=widgets/marathon.d.luau).
-- At runtime these are read-only globals provided by the preludes (core,
-- `service` module, `timer` module).

type Tone = "normal" | "muted" | "accent" | "warning" | "critical"
type Bind<T> = T | () -> T
type Node = { type: string }
type Child = Node | string | number | false | nil | () -> any
type Key = string | number

export type Signal<T> = typeof(setmetatable({}, {} :: {
	__call: (any) -> T,
})) & {
	get: (self: any) -> T,
	peek: (self: any) -> T,
	set: (self: any, value: T) -> (),
	update: (self: any, fn: (T) -> T) -> (),
}

-- A declarative event handler: the host calls a module, no extension code runs.
export type Action = { ["$action"]: { module: string, func: string, args: any } }
type On<P> = ((P) -> ()) | Action

export type CallResult = { ok: boolean, value: any?, error: string? }

export type Service = {
	-- Current value first, then every change; nil while the service is down.
	subscribe: (self: Service, path: string) -> Signal<any?>,
	call: (self: Service, method: string, args: any?, callback: ((CallResult) -> ())?) -> (),
	-- For event props; event data (e.g. scroll delta) arrives in the call args as `event`.
	action: (self: Service, method: string, args: { [string]: any }?) -> Action,
}

declare function signal<T>(initial: T): Signal<T>
-- Like `signal`, but the value survives hot reloads of this extension.
declare function persist<T>(key: string, initial: T): Signal<T>
declare function log(...: any): ()
-- Errors unless the manifest lists "service:<name>".
declare function use(name: string): Service
-- Capability "timer". Both return a function that cancels the timer; timers
-- stop with the code that started them. `every` is at least 16 ms apart.
declare function after(ms: number, fn: () -> ()): () -> ()
declare function every(ms: number, fn: () -> ()): () -> ()

type Events = {
	on_click: On<nil>?,
	on_press: On<nil>?,
	on_release: On<nil>?,
	on_scroll: On<{ delta: number }>?,
}

declare function Row(props: { key: Key?, gap: Bind<number>?, [number]: Child }): Node
declare function Column(props: { key: Key?, gap: Bind<number>?, [number]: Child }): Node
declare function Text(props: string | (() -> string) | { key: Key?, text: Bind<string>, tone: Bind<Tone>? }): Node
declare function Icon(props: string | { key: Key?, name: Bind<string>, tone: Bind<Tone>? }): Node
declare function Pill(props: string | (Events & { key: Key?, icon: Bind<string>?, label: Bind<string>, active: Bind<boolean>? })): Node
declare function Button(props: string | (Events & { key: Key?, label: Bind<string> })): Node
declare function Progress(props: number | { key: Key?, value: Bind<number>, label: Bind<string>? }): Node
declare function Spacer(props: { key: Key? }?): Node
-- marathon's own component (falls back to Icon + Progress).
declare function Meter(props: { key: Key?, icon: Bind<string>?, label: Bind<string>?, value: Bind<number> }): Node
