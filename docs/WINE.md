# WINE INFO

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
cp /home/sasq/projects/demarc/panic/tssoft32.acm ~/.wine/drive_c/windows/syswow64/tssoft32.acm
wine reg add 'HKLM\Software\Wow6432Node\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d 'tssoft32.acm' /f 2>&1 | tail -2;
wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d 'tssoft32.acm' /f 2>&1 | tail -2;
```

