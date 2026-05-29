## demarc
An command line emulator frontend for the demoscene
<table>
  <tr>
    <td>
      <img width="576" height="432" alt="Nexus 7-14" src="https://github.com/user-attachments/assets/42759244-7583-4cc3-a9a0-8cde0c3a8da2" />
    </td>
    <td>
      <img width="576" height="432" alt="Codeboys   Endians-75" src="https://github.com/user-attachments/assets/59d2562e-3be8-49ff-8d37-d2378bfd4b2c" />      
    </td>
  </tr>

</table>

**Main goal:**

Make it easy to watch demos from C64 and Amiga

* Runs multiple demos in order or shuffled
* Shows demo meta data as overlay
* CRT shader for "authentic" look
* Can run C64 images or exes
* Can run Amiga images, exes or directories
* Can run Atari ST images or exes
* Right-Alt hotkey for disk switch etc



## BUILD

You need _rust_.

`cargo build --release`

## RUN

You need libretro libraries for the emulated system. Libraries are searched for
in `<current_dir>/libretro`, `<exec_dir>` and `/usr/lib/libretro/`

then

`cargo run -- <files>`

or

`target/release/demarc <files>`

### Windows

Libraries are in `libretro/` 

If you copy the exe to your path, also copy the DLL:s and it should work

### Linux

If you installed retroarch you may have libs available in /usr/lib/libretro

### Mac OS

Remember that downloaded dylib files need their quarantine bit cleared

## SHORTCUTS

_Right Alt_ +
```
D = Swap disk
N = Next file
S = Change scaling
B = Change border
R = Reset
I = Toggle Info
P = Screenshot
C = Toggle CRT filter
```


