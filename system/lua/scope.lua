--! A two-channel oscilloscope over a spectrum display: the default picture for
--! a music file.
--!
--! This is a Luau script, reloaded automatically when you save it. Break it and
--! the last working version keeps running (check the log); delete `render` and
--! the window goes black.
--!
--! What the host provides:
--!
--!   WIDTH, HEIGHT           frame size, in pixels
--!   render(buf)             you define this; called once per frame
--!   init()                  optional; called once after (re)load
--!
--!   get_samples()           the audio being played right now, as interleaved
--!                           stereo floats in -1..1. Left is s[1], right s[2],
--!                           next pair s[3], s[4] ... so #s/2 pairs in all.
--!                           Delayed to match the speakers, not the samples the
--!                           emulator just rendered -- so the trace lines up
--!                           with what you hear.
--!   get_spectrum(bins)      `bins` magnitudes, roughly 0..1, log-spaced from
--!                           low to high. Computed once per frame however often
--!                           you ask.
--!   get_meta()              table of title / composer / format / channels /
--!                           sample_rate, as far as the player knows them.
--!   get_frame_count()       frames drawn since the song loaded
--!   get_time()              seconds of song played
--!
--!   rgb(r, g, b [, a])      pack a colour. Use this rather than writing a hex
--!                           literal: the byte order is not what you would
--!                           guess, and getting it wrong swaps red and blue.
--!   clear(buf, colour)      fill the frame
--!   box(buf, x, y, w, h, colour)
--!                           fill a rectangle, clipped to the frame
--!   load_font(name)         an 8x16 Amiga font -- "topaz" or "topaz1200".
--!                           Has .width and .height; load it in init().
--!   text(buf, font, x, y, s, colour)
--!                           draw a string with its top-left at x, y, clipped
--!                           to the frame. Only the lit pixels are written, so
--!                           it goes over whatever is already there.
--!
--! `buf` is a Luau buffer of WIDTH*HEIGHT*4 bytes. Anything the helpers above
--! don't cover, write yourself:
--!
--!   buffer.writeu32(buf, (y * WIDTH + x) * 4, colour)

local BACKGROUND = rgb(0x08, 0x08, 0x10)
-- Baseline, drawn at the vertical centre of each channel's band.
local AXIS = rgb(0x20, 0x20, 0x30)
local TRACE = { rgb(0x50, 0xe0, 0xa0), rgb(0xe0, 0x90, 0x50) }
-- Muted, and violet rather than either trace colour, so the bars read as a
-- backdrop and the waveform stays legible over them.
local BAR = rgb(0x3c, 0x12, 0x20)

-- Left in the top half of the frame, right in the bottom.
local BAND = HEIGHT // 2
local HALF = BAND // 2

-- One bar every BAR_STEP pixels, BAR_STEP-1 of them filled so the bars stay
-- visually separate.
local BAR_STEP = 10
local BINS = WIDTH // BAR_STEP
-- The quietest magnitude that still shows as a sliver. Chip tunes cover a wide
-- dynamic range, so the bars are scaled in dB; linear scaling leaves everything
-- but the loudest note flat against the floor.
local FLOOR_DB = -54

-- Topaz, the Workbench font: the right typeface for a music player that spends
-- its life showing Amiga and C64 tunes. 8x16 per character.
local FONT
local TEXT = rgb(0xd0, 0xd0, 0xe0)
-- Drawn a pixel down and right of the text in near-black, the cheap drop shadow
-- every Amiga demo used, so a title stays readable over a bright trace.
local SHADOW = rgb(0x00, 0x00, 0x08)
-- Margin, and the top-left of the first line.
local MARGIN = 6

-- Bar heights carried between frames. The FFT jumps around from frame to frame
-- at 60 Hz, which reads as a flicker; letting a bar rise instantly but fall
-- slowly turns that into a peak-decay meter.
local FALL = 0.02
local level = {}

function Init()
	for i = 1, BINS do
		level[i] = 0
	end
	FONT = load_font("topaz")
end

function math.clamp(x, min, max)
    return math.max(math.min(x, max), min)
end

-- Shadowed text, truncated to whatever fits on one line from `x`.
local function label(buf, x, y, s, colour)
	if s == nil or s == "" then
		return
	end
	local fits = (WIDTH - x) // FONT.width
	if #s > fits then
		s = string.sub(s, 1, fits)
	end
	text(buf, FONT, x + 1, y + 1, s, SHADOW)
	text(buf, FONT, x, y, s, colour or TEXT)
end

-- mm:ss, the way a tape counter would have shown it.
local function timestamp(seconds)
	local m = math.floor(seconds / 60)
	return string.format("%02d:%02d", m, math.floor(seconds) % 60)
end

---@param buf Buffer
function Render(buf)
	clear(buf, BACKGROUND)

	-- Spectrum first: everything else draws over it.
	local spectrum = get_spectrum(BINS)
	local bar_h = HEIGHT // 3
	for i = 1, BINS do
		-- log(0) is -inf, so floor the magnitude before taking it.
		local db = 20 * math.log(math.max(spectrum[i], 1e-6), 10)
		local mag = math.clamp((db - FLOOR_DB) / -FLOOR_DB, 0, 1)
		level[i] = math.max(mag, (level[i] or 0) - FALL)
		local h = math.floor(level[i] * bar_h)
		if h > 0 then
			box(buf, (i - 1) * BAR_STEP, HEIGHT - h, BAR_STEP - 1, h, BAR)
		end
	end

	local s = get_samples()
	local pairs_n = #s // 2

	for ch = 0, 1 do
		local top = ch * BAND
		-- Baseline first, so the trace draws over it.
		box(buf, 0, top + HALF, WIDTH, 1, AXIS)
		if pairs_n ~= 0 then
			local colour = TRACE[ch + 1]
			local prev = nil
			-- One column per horizontal pixel, sampling the frame's waveform at
			-- even intervals. A frame is ~735 pairs against 320 columns, so this
			-- decimates; the exact peaks matter less than the shape.
			for x = 0, WIDTH - 1 do
				-- +1 because Lua tables are 1-based, +ch to pick the channel out of
				-- the interleaved pair.
				local i = (x * pairs_n) // WIDTH * 2 + ch + 1
				local y = math.clamp(HALF - math.floor(s[i] * HALF), 0, BAND - 1)
				-- Join consecutive samples with a vertical span, so a fast waveform
				-- reads as a continuous trace rather than a dotted line.
				local from = prev or y
				local lo = math.min(from, y)
				box(buf, x, top + lo, 1, math.abs(y - from) + 1, colour)
				prev = y
			end
		end
	end

	-- Text last, so it stays legible over the trace it crosses.
	local meta = get_meta()
	label(buf, MARGIN, MARGIN, meta.title or "unknown")
	label(buf, MARGIN, MARGIN + FONT.height, meta.composer or "", rgb(0x90, 0x90, 0xb0))
	local clock = timestamp(get_time())
	label(buf, WIDTH - MARGIN - #clock * FONT.width, HEIGHT - MARGIN - FONT.height, clock)
end
