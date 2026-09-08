# WINE INFO

demarc runs every Windows release in `~/.wine-demarc` (`PREFIX_DIR` in
`src/wine.rs`) — so that is the prefix every command below wants `WINEPREFIX`
pointed at. Deliberately not the user's
own `~/.wine`: besides what a demo may install into it, a personal prefix on a
hidpi screen carries `LogPixels` 192, and wine then hands a non-DPI-aware demo
a screen half the size and blows the result up — the top-left quarter of the
picture, at double size, with nothing anywhere saying why. demarc puts its own
prefix back to 96 when it finds it moved.

## DEMO SETUP

### Wine modifications

#### Use GLX instead of EGL

`wine reg add 'HKCU\Software\Wine\X11 Driver' /v UseEGL /d N /f`

GLX is older and more tested.
For running 1995 / Kewlers on Nvidia hardware

#### DXVK

`winetricks dxvk`

winertricks installation warns, but works.

Fixes white screen in elevated

### Installing tssoft32.acm for Panic Room

```sh
cp /home/sasq/projects/demarc/panic/tssoft32.acm ~/.wine-demarc/drive_c/windows/syswow64/tssoft32.acm
wine reg add 'HKLM\Software\Wow6432Node\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d 'tssoft32.acm' /f 2>&1 | tail -2;
wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d 'tssoft32.acm' /f 2>&1 | tail -2;
```

