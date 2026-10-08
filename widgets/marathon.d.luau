-- Editor types for Luau widgets (luau-lsp: --definitions=widgets/marathon.d.luau).
-- At runtime these are read-only globals provided by the shell's prelude.

type Tone = "normal" | "muted" | "accent" | "warning" | "critical"
type Bind<T> = T | () -> T
type Node = { type: string }
type Child = Node | string | number | false | nil | () -> any

export type Signal<T> = typeof(setmetatable({}, {} :: {
	__call: (any) -> T,
})) & {
	get: (self: any) -> T,
	peek: (self: any) -> T,
	set: (self: any, value: T) -> (),
	update: (self: any, fn: (T) -> T) -> (),
}

export type CallResult = { ok: boolean, value: any?, error: string? }

export type Service = {
	-- Current value first, then every change; nil while the service is down.
	subscribe: (self: Service, path: string) -> Signal<any?>,
	call: (self: Service, method: string, args: any?, callback: ((CallResult) -> ())?) -> (),
}

declare function signal<T>(initial: T): Signal<T>
-- Errors unless `name` is listed in this widget's `uses` (widget.toml).
declare function use(name: string): Service
declare function log(...: any): ()

declare function Row(props: { gap: Bind<number>?, [number]: Child }): Node
declare function Column(props: { gap: Bind<number>?, [number]: Child }): Node
declare function Text(props: string | (() -> string) | { text: Bind<string>, tone: Bind<Tone>? }): Node
declare function Icon(props: string | { name: Bind<string> }): Node
declare function Pill(props: string | {
	icon: Bind<string>?,
	label: Bind<string>,
	active: Bind<boolean>?,
	on_click: (() -> ())?,
	on_scroll: (({ delta: number }) -> ())?,
}): Node
declare function Button(props: string | { label: Bind<string>, on_click: (() -> ())? }): Node
declare function Progress(props: number | { value: Bind<number>, label: Bind<string>? }): Node
declare function Spacer(props: {}?): Node
