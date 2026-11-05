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
__declspec(dllimport) NTSTATUS __stdcall LdrGetDllHandle(const WCHAR *path, ULONG flags,
                                                        const UNICODE_STRING *name, HMODULE *module);

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

extern void *extra_slots[];
extern const char *const extra_names[];

// kernel32 imports kernelbase, so it is only there once the demo calls in.
void *__stdcall resolve_extra(unsigned i)
{
    static WCHAR dll[] = u"kernel32.dll";
    UNICODE_STRING file = {sizeof(dll) - 2, sizeof(dll), dll};
    HMODULE kernel32 = 0;
    void *address = 0;
    unsigned short n = length(extra_names[i]);
    ANSI_STRING name = {n, n + 1, extra_names[i]};
    if (!LdrLoadDll(0, 0, &file, &kernel32))
        LdrGetProcedureAddress(kernel32, &name, 0, &address);
    extra_slots[i] = address;
    return address;
}

__declspec(dllimport) NTSTATUS __stdcall NtProtectVirtualMemory(void *process, void **addr, ULONG *size,
                                                               ULONG prot, ULONG *old);
__declspec(dllimport) void *__stdcall RtlAllocateHeap(void *heap, ULONG flags, ULONG size);
__declspec(dllimport) int __stdcall RtlFreeHeap(void *heap, ULONG flags, void *ptr);

#define PAGE_EXECUTE_READWRITE 0x40

// A demo passing HEAP_NO_SERIALIZE on the process heap races DXVK's and
// wine's threads on it (.kkrieger), so those calls lock anyway.
void *alloc_continue, *free_continue;

#define STRIP_NO_SERIALIZE(name, cont)                                         \
    __attribute__((naked)) static void name(void)                              \
    {                                                                          \
        __asm__("movl %fs:0x30, %eax\n"                                        \
                "movl 0x18(%eax), %eax\n"                                      \
                "cmpl 4(%esp), %eax\n"                                         \
                "jne 1f\n"                                                     \
                "andl $-2, 8(%esp)\n"                                          \
                "1: jmp *_" #cont "\n");                                       \
    }

STRIP_NO_SERIALIZE(alloc_hook, alloc_continue)
STRIP_NO_SERIALIZE(free_hook, free_continue)

// Over a `mov edi,edi` hot-patch prologue: a jmp to `hook` in the padding
// before it, and a short jmp back to that.
static void hotpatch(unsigned char *fn, void (*hook)(void), void **cont)
{
    if (fn[0] != 0x8b || fn[1] != 0xff)
        return;
    void *addr = fn - 5;
    ULONG size = 7, old;
    if (NtProtectVirtualMemory((void *)-1, &addr, &size, PAGE_EXECUTE_READWRITE, &old))
        return;
    *cont = fn + 2;
    fn[-5] = 0xe9;
    *(long *)(fn - 4) = (long)((unsigned char *)hook - fn);
    *(volatile unsigned short *)fn = 0xf9eb;
    addr = fn - 5;
    size = 7;
    NtProtectVirtualMemory((void *)-1, &addr, &size, old, &old);
}

// Windows' msvcrt has names wine keeps in ucrtbase only (Brainworm's
// `_libm_sse2_sqrt_precise`), so a lookup that fails in msvcrt retries there.
static void *(__stdcall *real_get_proc_address)(HMODULE module, const char *name);

static int is_msvcrt(HMODULE module)
{
    unsigned char *base = module;
    if (!base || ((unsigned long)base & 3))
        return 0;
    unsigned char *nt = base + *(long *)(base + 0x3c);
    unsigned exports = *(unsigned *)(nt + 0x78);
    if (!exports)
        return 0;
    const char *name = (const char *)base + *(unsigned *)(base + exports + 12);
    const char *want = "msvcrt.dll";
    for (; *want; name++, want++)
        if ((*name | 0x20) != *want)
            return 0;
    return !*name;
}

static void *__stdcall get_proc_address(HMODULE module, const char *name)
{
    void *address = real_get_proc_address(module, name);
    if (!address && (unsigned long)name > 0xffff && is_msvcrt(module)) {
        static WCHAR dll[] = u"ucrtbase.dll";
        UNICODE_STRING file = {sizeof(dll) - 2, sizeof(dll), dll};
        HMODULE ucrtbase = 0;
        if (!LdrLoadDll(0, 0, &file, &ucrtbase))
            address = real_get_proc_address(ucrtbase, name);
    }
    return address;
}

// kernel32's GetProcAddress is its own code, not an import of ours: jump
// from its first bytes to `get_proc_address`.
static void hook_kernel32_get_proc_address(void)
{
    static WCHAR dll[] = u"kernel32.dll";
    static const char proc[] = "GetProcAddress";
    UNICODE_STRING file = {sizeof(dll) - 2, sizeof(dll), dll};
    ANSI_STRING name = {sizeof(proc) - 1, sizeof(proc), proc};
    HMODULE kernel32 = 0;
    unsigned char *fn = 0;
    if (LdrGetDllHandle(0, 0, &file, &kernel32) ||
        LdrGetProcedureAddress(kernel32, &name, 0, (void **)&fn))
        return;
    void *addr = fn;
    ULONG size = 5, old;
    if (NtProtectVirtualMemory((void *)-1, &addr, &size, PAGE_EXECUTE_READWRITE, &old))
        return;
    fn[0] = 0xe9;
    *(long *)(fn + 1) = (long)((unsigned char *)get_proc_address - (fn + 5));
    addr = fn;
    size = 5;
    NtProtectVirtualMemory((void *)-1, &addr, &size, old, &old);
}

int __stdcall DllMain(HMODULE self, ULONG reason, void *reserved)
{
    (void)reserved;
    if (reason != DLL_PROCESS_ATTACH)
        return 1;
    LdrDisableThreadCalloutsForDll(self);
    hotpatch((unsigned char *)RtlAllocateHeap, alloc_hook, &alloc_continue);
    hotpatch((unsigned char *)RtlFreeHeap, free_hook, &free_continue);
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
    static const char proc[] = "GetProcAddress";
    ANSI_STRING name = {sizeof(proc) - 1, sizeof(proc), proc};
    if (!LdrGetProcedureAddress(real, &name, 0, (void **)&real_get_proc_address))
        hook_kernel32_get_proc_address();
    return 1;
}
