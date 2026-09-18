// winmm.dll with the export table of the Windows one, every export a jump into
// wine's own winmm. See README.md.

typedef unsigned short WCHAR;
typedef void *HMODULE;

__declspec(dllimport) HMODULE __stdcall LoadLibraryW(const WCHAR *name);
__declspec(dllimport) void *__stdcall GetProcAddress(HMODULE module, const char *name);
__declspec(dllimport) unsigned long __stdcall GetEnvironmentVariableW(const WCHAR *name, WCHAR *buf,
                                                                     unsigned long size);
__declspec(dllimport) int __stdcall DisableThreadLibraryCalls(HMODULE module);

typedef unsigned long DWORD;
typedef unsigned int UINT;
typedef struct WAVEHDR WAVEHDR;
typedef DWORD(__stdcall *OpenFn)(void **, UINT, void *, DWORD, DWORD, DWORD);
typedef DWORD(__stdcall *CloseFn)(void *);
typedef DWORD(__stdcall *HeaderFn)(void *, WAVEHDR *, UINT);
typedef int(__stdcall *DriverCallbackFn)(DWORD, DWORD, void *, DWORD, DWORD, DWORD, DWORD);

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

// Wine reads a WAVEHDR again every time it feeds the device; Windows latches it at
// waveOutWrite. Fairlight's "Uncovering Static" writes one from the stack and then
// reuses the stack, so wine is handed a shadow copy and the app's header only gets
// the flags back.
struct WAVEHDR {
    char *lpData;
    DWORD dwBufferLength, dwBytesRecorded, dwUser, dwFlags, dwLoops;
    WAVEHDR *lpNext;
    DWORD reserved;
};

typedef struct {
    WAVEHDR *volatile orig;
    WAVEHDR copy;
} Shadow;

typedef struct {
    volatile DWORD used;
    void *hwo;
    DWORD callback, instance, flags;
} Device;

#define CALLBACK_TYPEMASK 0x70000
#define CALLBACK_FUNCTION 0x30000
#define WAVE_FORMAT_QUERY 1
#define WOM_DONE 0x3BD
#define MMSYSERR_NOMEM 7

static Shadow shadows[1024];
static Device devices[32];
static OpenFn real_open;
static CloseFn real_close;
static HeaderFn real_prepare, real_unprepare, real_write;
static DriverCallbackFn driver_callback;

static Shadow *find_shadow(WAVEHDR *hdr)
{
    for (unsigned i = 0; i < sizeof(shadows) / sizeof(*shadows); i++)
        if (shadows[i].orig == hdr)
            return &shadows[i];
    return 0;
}

static void __stdcall wave_callback(void *hwo, UINT msg, Device *dev, DWORD p1, DWORD p2)
{
    if (msg == WOM_DONE) {
        Shadow *s = (Shadow *)((char *)p1 - __builtin_offsetof(Shadow, copy));
        s->orig->dwFlags = s->copy.dwFlags;
        p1 = (DWORD)s->orig;
    }
    driver_callback(dev->callback, dev->flags >> 16, hwo, msg, dev->instance, p1, p2);
}

static DWORD __stdcall wave_open(void **hwo, UINT id, void *fmt, DWORD callback, DWORD instance, DWORD flags)
{
    if (flags & WAVE_FORMAT_QUERY)
        return real_open(hwo, id, fmt, callback, instance, flags);
    Device *dev = 0;
    for (unsigned i = 0; i < sizeof(devices) / sizeof(*devices) && !dev; i++)
        if (__sync_bool_compare_and_swap(&devices[i].used, 0, 1))
            dev = &devices[i];
    if (!dev)
        return MMSYSERR_NOMEM;
    dev->callback = callback;
    dev->instance = instance;
    dev->flags = flags & CALLBACK_TYPEMASK;
    DWORD ret = real_open(hwo, id, fmt, (DWORD)wave_callback, (DWORD)dev,
                          (flags & ~CALLBACK_TYPEMASK) | CALLBACK_FUNCTION);
    if (ret)
        dev->used = 0;
    else
        dev->hwo = *hwo;
    return ret;
}

static DWORD __stdcall wave_close(void *hwo)
{
    DWORD ret = real_close(hwo);
    for (unsigned i = 0; i < sizeof(devices) / sizeof(*devices) && !ret; i++)
        if (devices[i].used && devices[i].hwo == hwo) {
            devices[i].hwo = 0;
            devices[i].used = 0;
        }
    return ret;
}

static DWORD __stdcall wave_prepare(void *hwo, WAVEHDR *hdr, UINT size)
{
    if (!hdr || size < sizeof(WAVEHDR))
        return real_prepare(hwo, hdr, size);
    Shadow *s = find_shadow(hdr);
    for (unsigned i = 0; i < sizeof(shadows) / sizeof(*shadows) && !s; i++)
        if (__sync_bool_compare_and_swap(&shadows[i].orig, 0, hdr))
            s = &shadows[i];
    if (!s)
        return MMSYSERR_NOMEM;
    s->copy = *hdr;
    DWORD ret = real_prepare(hwo, &s->copy, sizeof(WAVEHDR));
    hdr->dwFlags = s->copy.dwFlags;
    if (ret)
        s->orig = 0;
    return ret;
}

static DWORD __stdcall wave_unprepare(void *hwo, WAVEHDR *hdr, UINT size)
{
    Shadow *s = hdr ? find_shadow(hdr) : 0;
    if (!s)
        return real_unprepare(hwo, hdr, size);
    DWORD ret = real_unprepare(hwo, &s->copy, sizeof(WAVEHDR));
    hdr->dwFlags = s->copy.dwFlags;
    if (!ret)
        s->orig = 0;
    return ret;
}

static DWORD __stdcall wave_write(void *hwo, WAVEHDR *hdr, UINT size)
{
    Shadow *s = hdr ? find_shadow(hdr) : 0;
    if (!s)
        return real_write(hwo, hdr, size);
    s->copy.lpData = hdr->lpData;
    s->copy.dwBufferLength = hdr->dwBufferLength;
    s->copy.dwUser = hdr->dwUser;
    s->copy.dwFlags = hdr->dwFlags;
    s->copy.dwLoops = hdr->dwLoops;
    DWORD ret = real_write(hwo, &s->copy, sizeof(WAVEHDR));
    hdr->dwFlags = s->copy.dwFlags;
    return ret;
}

static int same(const char *a, const char *b)
{
    while (*a && *a == *b)
        a++, b++;
    return *a == *b;
}

static void *hook(const char *name, void *fn)
{
    for (unsigned i = 0; i < export_count; i++)
        if (same(names[i], name)) {
            void *real = slots[i];
            if (fn)
                slots[i] = fn;
            return real;
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
    driver_callback = hook("DriverCallback", 0);
    real_open = hook("waveOutOpen", wave_open);
    real_close = hook("waveOutClose", wave_close);
    real_prepare = hook("waveOutPrepareHeader", wave_prepare);
    real_unprepare = hook("waveOutUnprepareHeader", wave_unprepare);
    real_write = hook("waveOutWrite", wave_write);
    return 1;
}
