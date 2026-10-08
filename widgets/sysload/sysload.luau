-- CPU load and memory meters, fine-grained style: each prop that reads a
-- signal is a function, and only that prop re-runs when its signal changes.

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
	Icon "cpu",
	Progress {
		value = ratio(load),
		label = function()
			local v = load()
			return if v then string.format("%.2f", v.load1) else "…"
		end,
	},
	Icon "memory",
	Progress {
		value = ratio(mem),
		label = function()
			local m = mem()
			return if m then string.format("%.1f/%.0fG", m.used_gb, m.total_gb) else "…"
		end,
	},
}
