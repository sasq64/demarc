# d3dcompiler_47.dll

A 32-bit `d3dcompiler_47.dll` that keeps the result of each successful
`D3DCompile` and hands it back when the same source, macros, entry point, target
and flags come again. Every export goes to the native compiler, which
`system/win/setup-wine.sh` moves to `d3dcompiler_47_native.dll`. A call with an
include handler is not cached.

Wine's d2d1 compiles its shaders per render target. Candy (`zoo.42678`) makes a
render target per frame, so it compiled six shaders a frame and ran at 10 fps.

`just d3dcompiler_47` rebuilds `system/win/prefix/d3dcompiler_47.dll`; the prefix
already forces the native DLL.
