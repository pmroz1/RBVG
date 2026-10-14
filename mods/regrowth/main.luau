-- Example mod: a block broken by the player grows back after 3 seconds of simulation
-- time, unless the spot was filled meanwhile or is no longer loaded.
--
-- Demonstrates events + world queries + the scheduler + validated world commands.

local REGROW_SECONDS = 3.0

events.subscribe("block_broken", function(e)
	local p = e.position
	local block = e.block
	scheduler.after(REGROW_SECONDS, function()
		-- The chunk may have been unloaded while we waited; never force-load it.
		if not world.is_loaded(p) then
			return
		end
		-- Only restore an empty spot: never overwrite a block the player placed since.
		if world.get_block(p) == "core:air" then
			world.set_block(p, block)
			log.info(`{block} magically regrew at ({p.x}, {p.y}, {p.z})`)
		end
	end)
end)
