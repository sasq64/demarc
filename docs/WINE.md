
### Wine modifications

#### Use GLX instead of EGL

`wine reg add 'HKCU\Software\Wine\X11 Driver' /v UseEGL /d N /f`

GLX is older and more tested.
For running 1995 / Kewlers on Nvidia hardware

#### DXVK

`winetricks dxvk`

winertricks installation warns, but works.

Fixes white screen in elevated
