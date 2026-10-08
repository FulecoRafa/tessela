-- CPU load and memory meters. `Meter` is a component the shell registers
-- (apps/marathon); renderers that do not draw it get its fallback.
-- Fine-grained style: each prop that reads a signal is a function, and only
-- that prop re-runs when its signal changes.

local sys = use("sys")
local load = sys:subscribe("load")
local mem = sys:subscribe("memory")

local function ratio(sig)
	return function()
		local v = sig()
		return if v then v.ratio else 0
	end
end

return Row {
	gap = 4,
	Meter {
		icon = "cpu",
		value = ratio(load),
		label = function()
			local v = load()
			return if v then string.format("%.2f", v.load1) else "…"
		end,
	},
	Meter {
		icon = "memory",
		value = ratio(mem),
		label = function()
			local m = mem()
			return if m then string.format("%.1f/%.0fG", m.used_gb, m.total_gb) else "…"
		end,
	},
}
