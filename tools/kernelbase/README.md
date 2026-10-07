# kernelbase.dll

A 32-bit `kernelbase.dll` with the same exports and ordinals as wine's, every
export a jump into wine's builtin kernelbase (found through `WINEDLLDIR<n>`) and
resolved with `LdrGetProcedureAddress`, so forwarders become real code.

Crinkler resolves imports by reading kernelbase's export address table directly.
wine forwards 94 of kernelbase's exports to ntdll (`QueryPerformanceFrequency`,
`HeapAlloc`, `EnterCriticalSection`, ...), and for those the table holds the
forwarder string; quite's "yes we can" jumps into
`"ntdll.RtlQueryPerformanceFrequency"` and dies on the `outsb` that `n` decodes to.

kernel32 imports kernelbase, so this imports only ntdll. kernelbase is a
KnownDLL, so it has to be in `syswow64`; an app-dir copy is ignored.

`just kernelbase` rebuilds `files/kernelbase.dll` from the export table of the
installed wine. Rebuild when wine's kernelbase gains exports kernel32 imports.
`scripts/mk_wine_prefix.sh` installs it, demarc sets `kernelbase=n,b`.
