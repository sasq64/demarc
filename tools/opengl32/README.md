# opengl32.dll

A 32-bit `opengl32.dll` with the same exports and ordinals as wine's, every
export a jump into wine's builtin opengl32 (found through `WINEDLLDIR<n>`).

`wglGetProcAddress` falls back to opengl32's own exports when wine's returns
NULL. Wine only looks in its extension registry, so the GL 1.1 functions are
NULL there, while NVIDIA's Windows driver hands them out too. LJ's "Hexer"
fetches `glGenTextures` that way and calls the NULL.

`wglChoosePixelFormat` asks for 32 colour and 8 alpha bits when an RGBA
request of 24+ bits asks for no alpha, as Windows drivers hand out alpha
anyway. Kewlers' "Aesterozoa" quits when `GL_ALPHA_BITS` is 0.

`wglSetPixelFormat` swaps a single-buffered format for the double-buffered
one wine's `wglChoosePixelFormat` picks for the same descriptor. Front-buffer
drawing never reaches the session, and Alcatraz's "The Explorer" hard-codes
format 8, single-buffered in wine's list.

`wglSwapBuffers` empties the x87 register stack (`emms`) after the swap.
Gopher & LJ's "Yama" leaves one value on it per frame; on Windows that never
overflows, under wine the time turns NaN after a few frames.

`wglUseFontOutlinesA`/`W` with a format other than 0 or 1 goes to
`wglUseFontBitmaps` instead. Crinkler takes `wglUseFontBitmapsA` from a range
import by position, which lands two exports later in wine's table; Fulcrum's
"8K Reality" pushes its four arguments, and the eight popped left its stack
16 bytes off.

`wglBindTexImageARB` copies the pbuffer with a context of its own, made on
the pbuffer's DC and sharing with the caller's. wine's copies with an
internal context of another pixel format, a fatal GLX `BadMatch` on NVIDIA
(Plastic's "Your Fingers So Gently On My Skin").

Float pbuffers (`WGL_TYPE_RGBA_FLOAT_ARB`) are emulated: Xwayland's GLX has
no float fbconfigs, so wine finds no format and the pbuffer is never made.
`wglChoosePixelFormatARB` hands out a fake format, the pbuffer and its DC
are fake handles, a context made on that DC is made on the DC the pbuffer
came from, and making it current binds an FBO with an RGBA16F/32F texture.
`wglBindTexImageARB` copies that texture into the bound one with
`glCopyImageSubData`. Plastic's "Lady Of Dark Autumn" runs its simulation
in them.

`glShaderSource`, `glShaderSourceARB` and `glCreateShaderProgramv` (taken
through `wglGetProcAddress`) apply an override's `glsl` patches, which demarc
passes in `DEMARC_GLSL_PATCHES`. Every shader is logged to wine's stderr as
`opengl32: shader <hash>` (FNV-1a 64 of the source), and `patched shader
<hash>` when a patch changed it. A patch picks shaders by that hash or by a
regex match, then applies regex search/replace pairs. The regex engine in
`glsl.c` knows `. [] [^] \d \w \s \b ^ $ ( ) (?: ) |`, the quantifiers
`* + ? {n,m}` and their lazy `?` forms; replacements take `$0`-`$9` and `$$`.
`glsl_test.c` tests it natively and runs on every build. fr-080's ATI-era
GLSL is fixed this way.

`just opengl32` rebuilds `system/win/prefix/opengl32.dll`. `system/win/setup-wine.sh`
installs it, demarc sets `opengl32=n,b`.

`glAccum`, `glClearAccum` and `glClear`'s `GL_ACCUM_BUFFER_BIT` are emulated
when the context has no accumulation buffer, which no GLX config on NVIDIA's
Xwayland has: an RGBA32F texture behind an FBO, sized to the viewport at first
use, and each operation one full-screen pass. Fizzer's "The Real Party Is In
Your Pocket" accumulates 256 samples in it.

`glEdgeFlag` and `glEdgeFlagv` do nothing when `GL_RENDERER` names radeonsi
(checked in `wglMakeCurrent`). radeonsi (Mesa 26.2.5) faults and hangs the GPU
drawing display lists with edge flags in polygon mode `GL_LINE` (Stravaganza's
"This Way"). Its wireframes then show every triangle edge.

`glGenLists` hands out low list numbers: Mesa aligns a range of 32 or more to a
multiple of 32, while Windows drivers start at 1. Ctrl-Alt-Test's "D - Four"
makes its font with first glyph 1 and draws raw ASCII, so it needs list 1.

`glProgramParameteriEXT` is a no-op when wine has none: Mesa lacks
`EXT_geometry_shader4`, and Mercury's "Epsilon" quits on the NULL.

`glColorTableEXT` and `glTexImage2D` with `GL_COLOR_INDEX8_EXT` emulate
`EXT_paletted_texture`, which NVIDIA dropped: the indices stay in the proxy and
each level goes to GL as RGBA8, again whenever the palette changes. Interceptor
only needs the entry point; Kolor's "Relais" draws with it.

`wglChoosePixelFormatARB` retries without `WGL_SWAP_METHOD_ARB` when nothing
matches. Relais asks for `WGL_SWAP_COPY_ARB`, which no Xwayland format has.

`glTexImage2D` and `glRenderbufferStorage*` make an unsized `GL_DEPTH_COMPONENT`
`GL_DEPTH_COMPONENT24`. Mesa picks different formats for the two, so a blit of
colour and depth between them fails as a whole. Still's "The Seeker" resolves
its MSAA scene that way and was black on Mesa.
