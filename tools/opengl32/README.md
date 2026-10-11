# opengl32.dll

A 32-bit `opengl32.dll` with the same exports and ordinals as wine's, every
export a jump into wine's builtin opengl32 (found through `WINEDLLDIR<n>`).

`wglGetProcAddress` falls back to opengl32's own exports when wine's returns
NULL. Wine only looks in its extension registry, so the GL 1.1 functions are
NULL there, while NVIDIA's Windows driver hands them out too. LJ's "Hexer"
fetches `glGenTextures` that way and calls the NULL.

`just opengl32` rebuilds `files/opengl32.dll`. `scripts/mk_wine_prefix.sh`
installs it, demarc sets `opengl32=n,b`.
