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

`just opengl32` rebuilds `files/opengl32.dll`. `scripts/mk_wine_prefix.sh`
installs it, demarc sets `opengl32=n,b`.
