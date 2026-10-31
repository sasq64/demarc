# d3d10_1.dll

A 32-bit `d3d10_1.dll` with wine's exports. `D3D10CreateDevice1` and
`D3D10CreateDeviceAndSwapChain1` raise a 9.x feature level to 10.0; everything
goes to wine's builtin d3d10_1 (found through `WINEDLLDIR<n>`).

The usual Direct2D interop opens a D3D11 keyed-mutex texture on a D3D10.1
device created at level 9.1. Windows allows that, DXVK refuses it ("Cannot
create shared texture"), so `OpenSharedResource` returns NULL and Candy
(`zoo.42678`) faults on it.

`just d3d10_1` rebuilds `system/win/prefix/d3d10_1.dll`. `system/win/setup-wine.sh`
installs it, demarc sets `d3d10_1=n,b`.
