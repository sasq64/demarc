---@meta

WIDTH = 320
HEIGHT = 240

---@class Buffer
---@class Font
---@field width integer
---@field height integer

---@class Color

function Init() end

---@param buf Buffer
function Render(buf) end

---@return table
function get_meta() end

---@param r integer
---@param g integer
---@param b integer
---@param a? integer
---@return Color
-- Pack color components into pixel
function rgb(r, g, b, a) end

---@param x integer
---@param y integer
---@param w integer
---@param h integer
---@param col Color
---@param buf Buffer
function box(buf, x, y, w, h, col) end

---@param bins integer
---@return table
function get_spectrum(bins) end

---@param color Color
function clear(buf, color) end

---@return table
function get_samples() end

---@return Font
function load_font(name) end

---@param x integer
---@param y integer
---@param font Font
---@param color Color
---@param buf Buffer
function text(buf, font, x, y, text, color) end

---@return integer
function get_time() end
--------------------------------------------------------------------------------
-- Luau's built-in `buffer` library.
--
-- Not something src/music_vis.rs registers -- it comes with StdLib::BUFFER, and
-- the `Buffer` class above is Luau's native `buffer` type. Declared here only
-- because lua-language-server targets PUC-Lua and so has never heard of it.
--
-- Every offset is in BYTES from the start of the buffer, and every access is
-- bounds-checked: out of range raises "buffer access out of bounds" rather than
-- being clipped or ignored. Integer reads/writes are little-endian.
--------------------------------------------------------------------------------

buffer = {}

---@param size integer
---@return Buffer
-- A new, zero-filled buffer of `size` bytes.
function buffer.create(size) end

---@param str string
---@return Buffer
-- A new buffer holding the bytes of `str`.
function buffer.fromstring(str) end

---@param b Buffer
---@return string
-- The whole buffer as a byte string.
function buffer.tostring(b) end

---@param b Buffer
---@return integer
-- Size in bytes. For the frame buffer this is WIDTH * HEIGHT * 4.
function buffer.len(b) end

---@param b Buffer
---@param offset integer
---@return integer
function buffer.readi8(b, offset) end

---@param b Buffer
---@param offset integer
---@return integer
function buffer.readu8(b, offset) end

---@param b Buffer
---@param offset integer
---@return integer
function buffer.readi16(b, offset) end

---@param b Buffer
---@param offset integer
---@return integer
function buffer.readu16(b, offset) end

---@param b Buffer
---@param offset integer
---@return integer
function buffer.readi32(b, offset) end

---@param b Buffer
---@param offset integer
---@return integer
-- Read one pixel: offset (y * WIDTH + x) * 4.
function buffer.readu32(b, offset) end

---@param b Buffer
---@param offset integer
---@return number
function buffer.readf32(b, offset) end

---@param b Buffer
---@param offset integer
---@return number
function buffer.readf64(b, offset) end

---@param b Buffer
---@param offset integer
---@param value integer
function buffer.writei8(b, offset, value) end

---@param b Buffer
---@param offset integer
---@param value integer
function buffer.writeu8(b, offset, value) end

---@param b Buffer
---@param offset integer
---@param value integer
function buffer.writei16(b, offset, value) end

---@param b Buffer
---@param offset integer
---@param value integer
function buffer.writeu16(b, offset, value) end

---@param b Buffer
---@param offset integer
---@param value integer
function buffer.writei32(b, offset, value) end

---@param b Buffer
---@param offset integer
---@param value Color
-- Write one pixel: offset (y * WIDTH + x) * 4, value from rgb().
function buffer.writeu32(b, offset, value) end

---@param b Buffer
---@param offset integer
---@param value number
function buffer.writef32(b, offset, value) end

---@param b Buffer
---@param offset integer
---@param value number
function buffer.writef64(b, offset, value) end

---@param b Buffer
---@param offset integer
---@param count integer
---@return string
function buffer.readstring(b, offset, count) end

---@param b Buffer
---@param offset integer
---@param value string
---@param count? integer defaults to #value
function buffer.writestring(b, offset, value, count) end

---@param target Buffer
---@param targetOffset integer
---@param source Buffer
---@param sourceOffset? integer defaults to 0
---@param count? integer defaults to the rest of `source`
-- Overlapping ranges are handled correctly (memmove), so this can scroll a
-- frame within itself.
function buffer.copy(target, targetOffset, source, sourceOffset, count) end

---@param b Buffer
---@param offset integer
---@param value integer only the low byte is used
---@param count? integer defaults to the rest of the buffer
-- Fills BYTES, not pixels -- so it can only paint colours whose four channels
-- are equal. Use clear() or a writeu32 loop for anything else.
function buffer.fill(b, offset, value, count) end

---@param b Buffer
---@param bitOffset integer
---@param bitCount integer 0..32
---@return integer
function buffer.readbits(b, bitOffset, bitCount) end

---@param b Buffer
---@param bitOffset integer
---@param bitCount integer 0..32
---@param value integer
function buffer.writebits(b, bitOffset, bitCount, value) end
