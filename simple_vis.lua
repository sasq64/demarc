local BG_COL = rgb(0x08, 0x08, 0x10)
local BASELINE_COL = rgb(0x20, 0x20, 0x30)
local DRAW_COL = { rgb(0x50, 0xe0, 0xa0), rgb(0xe0, 0x90, 0x50) }
local TEXT_COL = rgb(0xd0, 0xd0, 0xe0)

local MARGIN = 6
local BAND = HEIGHT // 2
local HALF = BAND // 2

local FONT = load_font("topaz")

---@param buf Buffer
function Render(buf)
	clear(buf, BG_COL)
	local samples = get_samples()
	local count = #samples // 2

	for channel = 0, 1 do
		local top = channel * BAND
		box(buf, 0, top + HALF, WIDTH, 1, BASELINE_COL)
		if count ~= 0 then
			local colour = DRAW_COL[channel + 1]
			local prev = nil
			for x = 0, WIDTH - 1 do
				local i = (x * count) // WIDTH * 2 + channel + 1
				local y = math.clamp(HALF - math.floor(samples[i] * HALF), 0, BAND - 1)
				local from = prev or y
				local lo = math.min(from, y)
				box(buf, x, top + lo, 1, math.abs(y - from) + 1, colour)
				prev = y
			end
		end
	end

	local clock = get_time()
	local t = string.format("%02d:%02d", clock // 60, clock % 60)
	text(buf, FONT, MARGIN, HEIGHT - MARGIN - FONT.height, t, TEXT_COL)
end
