// winmm.dll with the export table of the Windows one, every export a jump into
// wine's own winmm. See README.md.

typedef unsigned short WCHAR;
typedef void *HMODULE;

__declspec(dllimport) HMODULE __stdcall LoadLibraryW(const WCHAR *name);
__declspec(dllimport) void *__stdcall GetProcAddress(HMODULE module, const char *name);
__declspec(dllimport) unsigned long __stdcall GetEnvironmentVariableW(const WCHAR *name, WCHAR *buf,
                                                                     unsigned long size);
__declspec(dllimport) int __stdcall DisableThreadLibraryCalls(HMODULE module);

// From thunks.S: one slot per export, preset to a stub that returns 0.
extern void *slots[];
extern const char *const names[];
extern const unsigned export_count;

#define DLL_PROCESS_ATTACH 1
#define PATH_MAX 1024

static void append(WCHAR *dst, unsigned *len, const WCHAR *src)
{
    while (*src && *len < PATH_MAX - 1)
        dst[(*len)++] = *src++;
    dst[*len] = 0;
}

// Wine hands every process its dll directories as WINEDLLDIR0, 1, ... in
// `\??\Z:\...` form; the real winmm is the i386 builtin in one of them.
static HMODULE load_wine_winmm(void)
{
    WCHAR var[] = u"WINEDLLDIR0";
    WCHAR path[PATH_MAX];

    for (WCHAR n = u'0'; n <= u'9'; n++) {
        var[10] = n;
        unsigned long got = GetEnvironmentVariableW(var, path, PATH_MAX);
        if (!got || got >= PATH_MAX)
            break;
        unsigned len = got;
        append(path, &len, u"\\i386-windows\\winmm.dll");
        const WCHAR *start = path;
        if (start[0] == u'\\' && start[1] == u'?' && start[2] == u'?' && start[3] == u'\\')
            start += 4;
        HMODULE module = LoadLibraryW(start);
        if (module)
            return module;
    }
    return 0;
}

int __stdcall DllMain(HMODULE self, unsigned long reason, void *reserved)
{
    if (reason != DLL_PROCESS_ATTACH)
        return 1;
    DisableThreadLibraryCalls(self);

    HMODULE real = load_wine_winmm();
    if (!real || real == self)
        return 0;
    for (unsigned i = 0; i < export_count; i++) {
        void *fn = GetProcAddress(real, names[i]);
        if (fn)
            slots[i] = fn;
    }
    return 1;
}
