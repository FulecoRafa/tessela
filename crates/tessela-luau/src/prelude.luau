-- Core prelude: signals, log and component constructors. Runs before module
-- preludes and before the sandbox is sealed; everything here becomes
-- read-only for the extension. `__host` and `__rt` are removed afterwards,
-- so extensions only reach the host through these wrappers.

local host = __host

-- Signal objects are empty proxies; their id lives here, out of the
-- extension's reach, so `s.id = 42` cannot retarget a signal.
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

-- Like `signal`, but the value survives reloads of this extension.
function persist(key, initial)
	return wrap(host.persist(key, initial))
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

for _, c in host.components do
	_G[c.name] = component(c.name, c.shorthand)
end

-- Private helpers for module preludes; removed before extension code runs.
__rt = {
	invoke = host.invoke,
	wrap = wrap,
	isSignal = function(v) return ids[v] ~= nil end,
	-- A declarative action: the host calls the module when the event fires,
	-- without running any extension code.
	action = function(module, func, args)
		return table.freeze({ ["$action"] = { module = module, func = func, args = args } })
	end,
}
