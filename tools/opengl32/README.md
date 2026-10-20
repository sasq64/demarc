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

`wglSwapBuffers` empties the x87 register stack (`emms`) after the swap.
Gopher & LJ's "Yama" leaves one value on it per frame; on Windows that never
overflows, under wine the time turns NaN after a few frames.

`wglBindTexImageARB` copies the pbuffer with a context of its own, made on
the pbuffer's DC and sharing with the caller's. wine's copies with an
internal context of another pixel format, a fatal GLX `BadMatch` on NVIDIA
(Plastic's "Your Fingers So Gently On My Skin").

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

`just opengl32` rebuilds `files/opengl32.dll`. `scripts/mk_wine_prefix.sh`
installs it, demarc sets `opengl32=n,b`.
