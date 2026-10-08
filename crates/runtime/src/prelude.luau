-- Runs once per widget, before the sandbox is sealed. Everything defined here
-- becomes read-only for the widget. `host` is captured as an upvalue and the
-- global is removed afterwards, so widgets only reach it through these wrappers.

local host = __host
-- Signal objects are empty proxies; their id lives here, out of the widget's
-- reach, so `s.id = 42` cannot retarget a signal.
local ids = setmetatable({}, { __mode = "k" })

local Signal = {}
Signal.__index = Signal
Signal.__metatable = "locked"
function Signal.__call(self) return host.get(ids[self]) end
function Signal.__tostring(self) return "signal#" .. tostring(ids[self]) end
function Signal.__newindex() error("signals are read-only; use :set()", 2) end
function Signal:get() return host.get(ids[self]) end
function Signal:peek() return host.peek(ids[self]) end
function Signal:set(v) host.set(ids[self], v) end
function Signal:update(f) host.set(ids[self], f(host.peek(ids[self]))) end

local function wrap(id)
	local s = setmetatable({}, Signal)
	ids[s] = id
	return s
end

function signal(initial)
	return wrap(host.new(initial))
end

-- `use("audio")` fails unless the widget manifest lists "audio".
function use(name)
	host.use(name)
	local svc = {}
	function svc:subscribe(path) return wrap(host.subscribe(name, path)) end
	function svc:call(method, args, cb) host.call(name, method, args, cb) end
	return table.freeze(svc)
end

function log(...)
	local parts = table.pack(...)
	for i = 1, parts.n do parts[i] = tostring(parts[i]) end
	host.log(table.concat(parts, " ", 1, parts.n))
end

local function component(kind, shorthand)
	return function(props)
		local out = { type = kind }
		if type(props) ~= "table" or ids[props] then
			-- `Text "hi"`, `Text(sig)` and `Text(function() ... end)` shorthands
			if props ~= nil and shorthand then out[shorthand] = props end
			props = {}
		end
		local children = props.children or {}
		for k, v in pairs(props) do
			if type(k) == "number" then
				children[k] = v
			elseif k ~= "children" then
				out[k] = v
			end
		end
		if #children > 0 then out.children = children end
		-- A signal passed directly as a prop binds to it.
		for k, v in pairs(out) do
			if ids[v] then
				local s = v
				out[k] = function() return s() end
			end
		end
		return out
	end
end

Row = component("Row")
Column = component("Column")
Text = component("Text", "text")
Icon = component("Icon", "name")
Pill = component("Pill", "label")
Button = component("Button", "label")
Progress = component("Progress", "value")
Spacer = component("Spacer")
