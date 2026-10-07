// kernelbase.dll with the export table of wine's, every export a jump into
// wine's own kernelbase, forwarders resolved to real code. See README.md.

typedef unsigned short WCHAR;
typedef unsigned short USHORT;
typedef unsigned long ULONG;
typedef long NTSTATUS;
typedef void *HMODULE;

typedef struct {
    USHORT Length, MaximumLength;
    WCHAR *Buffer;
} UNICODE_STRING;

typedef struct {
    USHORT Length, MaximumLength;
    const char *Buffer;
} ANSI_STRING;

__declspec(dllimport) NTSTATUS __stdcall LdrLoadDll(const WCHAR *path, ULONG flags,
                                                   const UNICODE_STRING *name, HMODULE *module);
__declspec(dllimport) NTSTATUS __stdcall LdrGetProcedureAddress(HMODULE module, const ANSI_STRING *name,
                                                               ULONG ordinal, void **address);
__declspec(dllimport) NTSTATUS __stdcall RtlQueryEnvironmentVariable_U(WCHAR *env, UNICODE_STRING *name,
                                                                      UNICODE_STRING *value);
__declspec(dllimport) NTSTATUS __stdcall LdrDisableThreadCalloutsForDll(HMODULE module);

// From thunks.S: one slot per export, preset to a stub that returns 0.
extern void *slots[];
extern const char *const names[];
extern const unsigned short ordinals[];
extern const unsigned export_count;

#define DLL_PROCESS_ATTACH 1
#define PATH_MAX 1024

static unsigned length(const char *s)
{
    unsigned n = 0;
    while (s[n])
        n++;
    return n;
}

// Wine hands every process its dll directories as WINEDLLDIR0, 1, ... in
// `\??\Z:\...` form; the real kernelbase is the i386 builtin in one of them.
static HMODULE load_wine_kernelbase(void)
{
    static const WCHAR suffix[] = u"\\i386-windows\\kernelbase.dll";
    WCHAR var[] = u"WINEDLLDIR0";
    WCHAR path[PATH_MAX];

    for (WCHAR n = u'0'; n <= u'9'; n++) {
        var[10] = n;
        UNICODE_STRING name = {sizeof(var) - 2, sizeof(var), var};
        UNICODE_STRING value = {0, sizeof(path) - sizeof(suffix), path};
        if (RtlQueryEnvironmentVariable_U(0, &name, &value))
            break;
        unsigned len = value.Length / 2;
        for (unsigned i = 0; i < sizeof(suffix) / 2; i++)
            path[len + i] = suffix[i];
        WCHAR *start = path;
        if (start[0] == u'\\' && start[1] == u'?' && start[2] == u'?' && start[3] == u'\\')
            start += 4;
        unsigned bytes = (len + sizeof(suffix) / 2 - 1 - (start - path)) * 2;
        UNICODE_STRING file = {bytes, bytes + 2, start};
        HMODULE module = 0;
        if (!LdrLoadDll(0, 0, &file, &module))
            return module;
    }
    return 0;
}

int __stdcall DllMain(HMODULE self, ULONG reason, void *reserved)
{
    (void)reserved;
    if (reason != DLL_PROCESS_ATTACH)
        return 1;
    LdrDisableThreadCalloutsForDll(self);
    HMODULE real = load_wine_kernelbase();
    if (!real)
        return 0;
    for (unsigned i = 0; i < export_count; i++) {
        void *address = 0;
        if (names[i]) {
            unsigned short n = length(names[i]);
            ANSI_STRING name = {n, n + 1, names[i]};
            LdrGetProcedureAddress(real, &name, 0, &address);
        } else {
            LdrGetProcedureAddress(real, 0, ordinals[i], &address);
        }
        if (address)
            slots[i] = address;
    }
    return 1;
}
