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