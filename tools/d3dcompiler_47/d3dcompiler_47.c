// d3dcompiler_47.dll that remembers what D3DCompile returned, everything else a
// jump into the native compiler. See README.md.

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
__declspec(dllimport) NTSTATUS __stdcall LdrDisableThreadCalloutsForDll(HMODULE module);

// From thunks.S: one slot per export, preset to a stub that returns 0.
extern void *slots[];
extern const char *const names[];
extern const unsigned export_count;

#define DLL_PROCESS_ATTACH 1
#define CACHE_SIZE 1024

typedef struct {
    const char *name, *definition;
} Macro;

typedef struct Blob Blob;
struct Blob {
    struct {
        void *query_interface;
        ULONG(__stdcall *add_ref)(Blob *blob);
    } *vtbl;
};

typedef HRESULT(__stdcall *Compile)(const void *data, unsigned size, const char *filename,
                                    const Macro *defines, void *include, const char *entry,
                                    const char *target, unsigned flags1, unsigned flags2,
                                    Blob **code, Blob **errors);

static Compile native_compile;

static struct {
    unsigned long long key;
    Blob *code;
} cache[CACHE_SIZE];
static unsigned cached;
static volatile long lock;

static void take_lock(void)
{
    while (__atomic_exchange_n(&lock, 1, __ATOMIC_ACQUIRE))
        __builtin_ia32_pause();
}

static void drop_lock(void) { __atomic_store_n(&lock, 0, __ATOMIC_RELEASE); }

static unsigned long long hash_bytes(unsigned long long hash, const void *data, unsigned size)
{
    const unsigned char *bytes = data;
    for (unsigned i = 0; i < size; i++) {
        hash ^= bytes[i];
        hash *= 0x100000001b3ull;
    }
    return hash;
}

static unsigned long long hash_string(unsigned long long hash, const char *s)
{
    if (!s)
        return hash_bytes(hash, "", 1);
    unsigned n = 0;
    while (s[n])
        n++;
    return hash_bytes(hash, s, n + 1);
}

static unsigned long long make_key(const void *data, unsigned size, const char *filename,
                                   const Macro *defines, const char *entry, const char *target,
                                   unsigned flags1, unsigned flags2)
{
    unsigned long long hash = hash_bytes(0xcbf29ce484222325ull, data, size);
    hash = hash_string(hash, filename);
    for (; defines && defines->name; defines++) {
        hash = hash_string(hash, defines->name);
        hash = hash_string(hash, defines->definition);
    }
    hash = hash_string(hash, entry);
    hash = hash_string(hash, target);
    hash = hash_bytes(hash, &flags1, sizeof(flags1));
    return hash_bytes(hash, &flags2, sizeof(flags2));
}

// An include handler brings in text the key cannot see.
static HRESULT __stdcall compile(const void *data, unsigned size, const char *filename,
                                 const Macro *defines, void *include, const char *entry,
                                 const char *target, unsigned flags1, unsigned flags2, Blob **code,
                                 Blob **errors)
{
    if (include || !code)
        return native_compile(data, size, filename, defines, include, entry, target, flags1,
                              flags2, code, errors);

    unsigned long long key = make_key(data, size, filename, defines, entry, target, flags1, flags2);
    Blob *found = 0;
    take_lock();
    for (unsigned i = 0; i < cached && !found; i++)
        if (cache[i].key == key)
            found = cache[i].code;
    drop_lock();
    if (found) {
        found->vtbl->add_ref(found);
        *code = found;
        if (errors)
            *errors = 0;
        return 0;
    }

    HRESULT result = native_compile(data, size, filename, defines, include, entry, target, flags1,
                                    flags2, code, errors);
    if (result >= 0 && *code) {
        take_lock();
        if (cached < CACHE_SIZE) {
            (*code)->vtbl->add_ref(*code);
            cache[cached].key = key;
            cache[cached].code = *code;
            cached++;
        }
        drop_lock();
    }
    return result;
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

int __stdcall DllMain(HMODULE self, ULONG reason, void *reserved)
{
    (void)reserved;
    if (reason != DLL_PROCESS_ATTACH)
        return 1;
    LdrDisableThreadCalloutsForDll(self);
    static WCHAR dll[] = u"d3dcompiler_47_native.dll";
    UNICODE_STRING file = {sizeof(dll) - 2, sizeof(dll), dll};
    HMODULE native = 0;
    if (LdrLoadDll(0, 0, &file, &native))
        return 0;
    for (unsigned i = 0; i < export_count; i++) {
        void *address = lookup(native, names[i]);
        if (equal(names[i], "D3DCompile")) {
            native_compile = (Compile)address;
            address = (void *)compile;
        }
        if (address)
            slots[i] = address;
    }
    return 1;
}
