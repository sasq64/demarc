// Stand-in for unrar's `isnt.cpp`, needed only when cross-compiling to Windows.
//
// `unrar_sys`' build script selects its source list with `cfg!(windows)`, which
// in a build script describes the *host*, not the target. Cross-compiling from
// Linux therefore drops `vendor/unrar/isnt.cpp` from the build while the rest of
// unrar still calls into it:
//
//   lld-link: error: undefined symbol: unsigned long __cdecl WinNT(void)
//   lld-link: error: undefined symbol: bool __cdecl IsWindows11OrGreater(void)
//
// The signatures below match `vendor/unrar/isnt.hpp` exactly so the MSVC-mangled
// names line up. Both are only used for OS-version feature checks (long-path
// handling, reserved device names, local-time conversion).
//
// Upstream reads the version with `GetVersionEx`, which reports 6.2 on anything
// newer than Windows 8 unless the executable carries a compatibility manifest,
// and then papers over that with a WMI query. `RtlGetVersion` reports the real
// version with no manifest and no COM, so we use it directly and skip the WMI
// path entirely. ntdll is already on the link line via the Rust standard library.
//
// See scripts/prepare-xwin.sh for the rest of the unrar cross-compile fixes.

#include <windows.h>

namespace {

const RTL_OSVERSIONINFOW &OsVersion()
{
  static RTL_OSVERSIONINFOW Info = []
  {
    RTL_OSVERSIONINFOW V = {};
    V.dwOSVersionInfoSize = sizeof(V);
    typedef NTSTATUS(WINAPI * RtlGetVersionPtr)(PRTL_OSVERSIONINFOW);
    HMODULE Ntdll = GetModuleHandleW(L"ntdll.dll");
    RtlGetVersionPtr RtlGetVersionFn =
        Ntdll == NULL ? NULL
                      : (RtlGetVersionPtr)GetProcAddress(Ntdll, "RtlGetVersion");
    if (RtlGetVersionFn == NULL || RtlGetVersionFn(&V) != 0)
    {
      // Nothing sensible left to try; claim Windows 10, the oldest version this
      // build targets. Every unrar caller treats a higher value as "supported".
      V.dwMajorVersion = 10;
      V.dwMinorVersion = 0;
      V.dwBuildNumber = 0;
    }
    return V;
  }();
  return Info;
}

} // namespace

// Packed major/minor, matching the WNT_* constants in isnt.hpp (e.g. 0x0a00).
DWORD WinNT()
{
  const RTL_OSVERSIONINFOW &V = OsVersion();
  return V.dwMajorVersion * 0x100 + V.dwMinorVersion;
}

bool IsWindows11OrGreater()
{
  const RTL_OSVERSIONINFOW &V = OsVersion();
  return V.dwMajorVersion > 10 ||
         (V.dwMajorVersion == 10 && V.dwBuildNumber >= 22000);
}
