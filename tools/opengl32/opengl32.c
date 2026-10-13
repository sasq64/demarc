// opengl32.dll with the export table of wine's, every export a jump into
// wine's own opengl32, wglGetProcAddress also finding the GL 1.1 exports.
// See README.md.

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

static int equal(const char *a, const char *b)
{
    while (*a && *a == *b)
        a++, b++;
    return *a == *b;
}

static void *lookup(HMODULE module, const char *name)
{
    void *address = 0;
    unsigned short n = length(name);
    ANSI_STRING s = {n, n + 1, name};
    LdrGetProcedureAddress(module, &s, 0, &address);
    return address;
}

// Wine hands every process its dll directories as WINEDLLDIR0, 1, ... in
// `\??\Z:\...` form; the real opengl32 is the i386 builtin in one of them.
static HMODULE load_wine_opengl32(void)
{
    static const WCHAR suffix[] = u"\\i386-windows\\opengl32.dll";
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

static HMODULE wine;
static void *(__stdcall *wine_get_proc_address)(const char *name);

// NVIDIA's driver hands out the GL 1.1 functions too, and Hexer relies on it.
static void *__stdcall get_proc_address(const char *name)
{
    void *address = wine_get_proc_address(name);
    return address ? address : lookup(wine, name);
}

// Windows drivers list their 32-bit formats with alpha first, so a request
// for none still gets 8; Aesterozoa quits when GL_ALPHA_BITS is 0.
static int(__stdcall *wine_choose_pixel_format)(void *hdc, const unsigned char *pfd);

static int __stdcall choose_pixel_format(void *hdc, const unsigned char *pfd)
{
    if (pfd && pfd[8] == 0 && pfd[9] >= 24 && pfd[18] == 0) {
        unsigned char with_alpha[40];
        for (unsigned i = 0; i < sizeof(with_alpha); i++)
            with_alpha[i] = pfd[i];
        with_alpha[9] = 32;
        with_alpha[18] = 8;
        int format = wine_choose_pixel_format(hdc, with_alpha);
        if (format)
            return format;
    }
    return wine_choose_pixel_format(hdc, pfd);
}

// Windows leaves the x87 stack empty after a swap. Yama leaks one value per
// frame and gets a NaN time once it overflows.
static int(__stdcall *wine_swap_buffers)(void *hdc);

static int __stdcall swap_buffers(void *hdc)
{
    int ret = wine_swap_buffers(hdc);
    __asm__ volatile("emms");
    return ret;
}

int __stdcall DllMain(HMODULE self, ULONG reason, void *reserved)
{
    (void)reserved;
    if (reason != DLL_PROCESS_ATTACH)
        return 1;
    LdrDisableThreadCalloutsForDll(self);
    wine = load_wine_opengl32();
    if (!wine)
        return 0;
    for (unsigned i = 0; i < export_count; i++) {
        void *address = 0;
        if (names[i]) {
            address = lookup(wine, names[i]);
            if (address && equal(names[i], "wglGetProcAddress")) {
                wine_get_proc_address = address;
                address = get_proc_address;
            } else if (address && equal(names[i], "wglChoosePixelFormat")) {
                wine_choose_pixel_format = address;
                address = choose_pixel_format;
            } else if (address && equal(names[i], "wglSwapBuffers")) {
                wine_swap_buffers = address;
                address = swap_buffers;
            }
        } else {
            LdrGetProcedureAddress(wine, 0, ordinals[i], &address);
        }
        if (address)
            slots[i] = address;
    }
    return 1;
}
