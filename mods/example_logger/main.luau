-- Example mod: logs block events and turns every 10th broken block into stone.

local broken = 0

events.subscribe("block_broken", function(e)
	broken += 1
	local p = e.position
	log.info(`{e.block} broken at ({p.x}, {p.y}, {p.z}); total broken: {broken}`)
	if broken % 10 == 0 then
		log.info("every 10th block comes back as stone")
		world.set_block(p, "core:stone")
	end
end)

events.subscribe("block_placed", function(e)
	local p = e.position
	log.info(`{e.block} placed at ({p.x}, {p.y}, {p.z})`)
end)

log.info(`loaded (api v{rbvg.api_version})`)
