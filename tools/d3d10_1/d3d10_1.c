// d3d10_1.dll that raises 9.x feature levels to 10.0, everything else a jump
// into wine's own d3d10_1. See README.md.

typedef unsigned short WCHAR;
typedef unsigned short USHORT;
typedef unsigned long ULONG;
typedef long NTSTATUS;
typedef long HRESULT;
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
extern const unsigned export_count;

#define DLL_PROCESS_ATTACH 1
#define PATH_MAX 1024
#define LEVEL_10_0 0xa000

typedef HRESULT(__stdcall *CreateDevice)(void *adapter, int type, HMODULE swrast, unsigned flags,
                                         int level, unsigned sdk, void **device);
typedef HRESULT(__stdcall *CreateDeviceAndSwapChain)(void *adapter, int type, HMODULE swrast,
                                                     unsigned flags, int level, unsigned sdk,
                                                     void *desc, void **swapchain, void **device);

static CreateDevice wine_create_device;
static CreateDeviceAndSwapChain wine_create_device_and_swapchain;

static int raised(int level) { return level < LEVEL_10_0 ? LEVEL_10_0 : level; }

static HRESULT __stdcall create_device(void *adapter, int type, HMODULE swrast, unsigned flags,
                                       int level, unsigned sdk, void **device)
{
    return wine_create_device(adapter, type, swrast, flags, raised(level), sdk, device);
}

static HRESULT __stdcall create_device_and_swapchain(void *adapter, int type, HMODULE swrast,
                                                     unsigned flags, int level, unsigned sdk,
                                                     void *desc, void **swapchain, void **device)
{
    return wine_create_device_and_swapchain(adapter, type, swrast, flags, raised(level), sdk, desc,
                                            swapchain, device);
}

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
// `\??\Z:\...` form; the real d3d10_1 is the i386 builtin in one of them.
static HMODULE load_wine_d3d10_1(void)
{
    static const WCHAR suffix[] = u"\\i386-windows\\d3d10_1.dll";
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
    HMODULE wine = load_wine_d3d10_1();
    if (!wine)
        return 0;
    for (unsigned i = 0; i < export_count; i++) {
        void *address = lookup(wine, names[i]);
        if (equal(names[i], "D3D10CreateDevice1")) {
            wine_create_device = (CreateDevice)address;
            address = (void *)create_device;
        } else if (equal(names[i], "D3D10CreateDeviceAndSwapChain1")) {
            wine_create_device_and_swapchain = (CreateDeviceAndSwapChain)address;
            address = (void *)create_device_and_swapchain;
        }
        if (address)
            slots[i] = address;
    }
    return 1;
}
