# dx8vb.dll

A 32-bit `dx8vb.dll` with the same exports and ordinals as wine's. The
`VB_D3DX*` math functions are real; everything else jumps into wine's builtin
dx8vb (found through `WINEDLLDIR<n>`).

Some Crinkler intros take their D3DX math from `dx8vb.dll` — on every Windows
since XP, unlike a versioned `d3dx9_xx.dll`. Wine's are all stubs, so Calodox'
"synchroplastikum" dies on `VB_D3DXVec3CatmullRom`.

Each `VB_D3DXFoo` has the arguments of `D3DXFoo` and jumps into d3dx9_43's
(`VB_D3DXMatrixfDeterminant` is `D3DXMatrixDeterminant`). d3dx9 has the
simple ones (`Add`, `Dot`, `Lerp`, ...) only as inlines, so `dx8vb.c` has those.

`just dx8vb` rebuilds `files/dx8vb.dll`. `scripts/mk_wine_prefix.sh` installs
it, demarc sets `dx8vb=n,b`.
