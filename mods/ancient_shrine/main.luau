-- Self-contained example: interact (E) with an altar to start a ritual. After a delay the
-- shrine turns the ground around it into glowing blocks. The engine knows nothing about
-- altars; it only provides interact events, the scheduler, world queries/commands and
-- ui.notify.

local ALTAR = "ancient_shrine:altar"
local GLOW = "ancient_shrine:glowstone"
local RITUAL_SECONDS = 3.0
local active = {}

events.subscribe("interact", function(e)
	if e.block ~= ALTAR then
		return
	end
	local p = e.position
	local key = `{p.x},{p.y},{p.z}`
	if active[key] then
		ui.notify("The ritual is already underway...")
		return
	end
	active[key] = true
	ui.notify("The ancient shrine awakens...")
	scheduler.after(RITUAL_SECONDS, function()
		active[key] = nil
		-- Never force-load; the player may have walked away or the altar was removed.
		if not world.is_loaded(p) or world.get_block(p) ~= ALTAR then
			return
		end
		for dx = -1, 1 do
			for dz = -1, 1 do
				if dx ~= 0 or dz ~= 0 then
					local q = { x = p.x + dx, y = p.y - 1, z = p.z + dz }
					if world.is_loaded(q) and world.get_block(q) ~= "core:air" then
						world.set_block(q, GLOW)
					end
				end
			end
		end
		ui.notify("The shrine's light spreads.")
	end)
end)
