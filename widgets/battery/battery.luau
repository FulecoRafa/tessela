-- Battery pill, React style: returning a function makes the whole widget one
-- binding that re-renders whenever anything it read changes. Data comes from
-- the Go `power` service.

local power = use("power")
local bat = power:subscribe("battery")

return function()
	local b = bat()
	if not b then
		return Pill { icon = "warning", label = "sem energia" }
	end
	return Pill {
		icon = if b.charging then "charging" else "battery",
		label = `{b.percent}%{if b.simulated then "~" else ""}`,
		active = b.charging,
	}
end
