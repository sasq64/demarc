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

// Wine's midiOut goes to the host's ALSA sequencer, where nothing plays it, so
// MIDI output is played by DirectMusic's software synth with gm.dls instead.
typedef struct {
    DWORD a;
    unsigned short b, c;
    unsigned char d[8];
} GUID;
typedef long HRESULT;
typedef long long REFERENCE_TIME;
#define VCALL(obj, n, type, ...) ((type)(*(void ***)(obj))[n])((obj), ##__VA_ARGS__)

typedef struct {
    DWORD dwSize, dwValidData;
    GUID guidObject, guidClass;
    DWORD ftDate[2], vVersion[2];
    WCHAR wszName[64], wszCategory[64], wszFileName[260];
    long long llMemLength;
    void *pbMemData, *pStream;
} DMUS_OBJECTDESC;

typedef struct {
    char *lpData;
    DWORD dwBufferLength, dwBytesRecorded, dwUser, dwFlags;
    void *lpNext;
    DWORD reserved, dwOffset, dwReserved[8];
} MIDIHDR;

static const GUID CLSID_DirectMusic = {0x636b9f10, 0x0c7d, 0x11d1, {0x95, 0xb2, 0x00, 0x20, 0xaf, 0xdc, 0x74, 0x21}};
static const GUID IID_IDirectMusic8 = {0x2d3629f7, 0x813d, 0x4939, {0x85, 0x08, 0xf0, 0x5c, 0x6b, 0x75, 0xfd, 0x97}};
static const GUID CLSID_DirectMusicLoader = {0xd2ac2892, 0xb39b, 0x11d1, {0x87, 0x04, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd}};
static const GUID IID_IDirectMusicLoader8 = {0x19e7c08c, 0x0a44, 0x4e6a, {0xa1, 0x16, 0x59, 0x5a, 0x7c, 0xd5, 0xde, 0x8c}};
static const GUID IID_IDirectMusicCollection = {0xd2ac287c, 0xb39b, 0x11d1, {0x87, 0x04, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd}};
static const GUID GUID_NULL;
static DMUS_OBJECTDESC default_gm_collection = {
    sizeof(DMUS_OBJECTDESC),
    3,
    {0xf17e8673, 0xc3b4, 0x11d1, {0x87, 0x0b, 0x00, 0x60, 0x08, 0x93, 0xb1, 0xbd}},
    {0x480ff4b0, 0x28b2, 0x11d1, {0xbe, 0xf7, 0x00, 0xc0, 0x4f, 0xbf, 0x8f, 0xef}},
};

#define MOM_OPEN 0x3C7
#define MOM_CLOSE 0x3C8
#define MOM_DONE 0x3C9
#define MHDR_DONE 1
#define MHDR_PREPARED 2
#define MMSYSERR_ERROR 1

typedef struct {
    volatile long lock;
    int opened;
    void *port, *buffer, *clock;
    DWORD callback, instance, flags;
    unsigned char status;
} Synth;

static Synth synth;
static DWORD(__stdcall *real_midi_open)(void **, UINT, void *, DWORD, DWORD);
static void *real_midi_short, *real_midi_long, *real_midi_prepare, *real_midi_unprepare, *real_midi_reset,
    *real_midi_close;
static void *volatile synth_ready;
static void *volatile synth_done;

__declspec(dllimport) void *__stdcall CreateThread(void *, DWORD, DWORD(__stdcall *)(void *), void *, DWORD,
                                                   DWORD *);
__declspec(dllimport) void *__stdcall CreateEventW(void *, int, int, const WCHAR *);
__declspec(dllimport) int __stdcall SetEvent(void *);
__declspec(dllimport) DWORD __stdcall WaitForSingleObject(void *, DWORD);

// DirectMusic objects are created in a thread of its own so the app's COM
// apartment is left alone; the thread stays in the MTA to keep them alive.
static DWORD __stdcall synth_thread(void *arg)
{
    (void)arg;
    HMODULE ole32 = LoadLibraryW(u"ole32.dll");
    HRESULT(__stdcall * co_init)(void *, DWORD) = ole32 ? GetProcAddress(ole32, "CoInitializeEx") : 0;
    HRESULT(__stdcall * co_create)(const GUID *, void *, DWORD, const GUID *, void **) =
        ole32 ? GetProcAddress(ole32, "CoCreateInstance") : 0;
    void *dm = 0, *loader = 0, *collection = 0, *port = 0, *buffer = 0, *clock = 0;
    if (co_create && co_init(0, 0) >= 0 &&
        co_create(&CLSID_DirectMusic, 0, 1, &IID_IDirectMusic8, &dm) >= 0 &&
        VCALL(dm, 11, HRESULT(__stdcall *)(void *, void *, void *), 0, 0) >= 0) {
        DWORD params[9] = {sizeof(params), 2, 0, 1};
        DWORD desc[7] = {sizeof(desc)};
        desc[6] = 4096;
        if (VCALL(dm, 5, HRESULT(__stdcall *)(void *, const GUID *, DWORD *, void **, void *), &GUID_NULL, params,
                  &port, 0) >= 0 &&
            VCALL(dm, 4, HRESULT(__stdcall *)(void *, DWORD *, void **, void *), desc, &buffer, 0) >= 0 &&
            VCALL(port, 8, HRESULT(__stdcall *)(void *, void **), &clock) >= 0 &&
            co_create(&CLSID_DirectMusicLoader, 0, 1, &IID_IDirectMusicLoader8, &loader) >= 0) {
            if (VCALL(loader, 3, HRESULT(__stdcall *)(void *, DMUS_OBJECTDESC *, const GUID *, void **),
                      &default_gm_collection,
                      &IID_IDirectMusicCollection, &collection) >= 0) {
                WCHAR name[64];
                DWORD patch;
                for (DWORD i = 0; !VCALL(collection, 4, HRESULT(__stdcall *)(void *, DWORD, DWORD *, WCHAR *, DWORD),
                                         i, &patch, name, 64);
                     i++) {
                    void *instrument = 0, *downloaded = 0;
                    if (VCALL(collection, 3, HRESULT(__stdcall *)(void *, DWORD, void **), patch, &instrument) >= 0)
                        VCALL(port, 6, HRESULT(__stdcall *)(void *, void *, void **, void *, DWORD), instrument,
                              &downloaded, 0, 0);
                }
            }
            VCALL(port, 15, HRESULT(__stdcall *)(void *, int), 1);
            synth.port = port;
            synth.buffer = buffer;
            synth.clock = clock;
        }
    }
    SetEvent(synth_done);
    if (synth.port)
        WaitForSingleObject(CreateEventW(0, 1, 0, 0), 0xFFFFFFFF);
    return 0;
}

static int synth_start(void)
{
    if (__sync_bool_compare_and_swap(&synth_ready, 0, (void *)1)) {
        synth_done = CreateEventW(0, 1, 0, 0);
        CreateThread(0, 0, synth_thread, 0, 0, 0);
    }
    while (!synth_done)
        ;
    WaitForSingleObject(synth_done, 0xFFFFFFFF);
    return synth.port != 0;
}

static void synth_send(DWORD msg, const unsigned char *data, DWORD len)
{
    while (!__sync_bool_compare_and_swap(&synth.lock, 0, 1))
        ;
    REFERENCE_TIME now = 0;
    VCALL(synth.clock, 3, HRESULT(__stdcall *)(void *, REFERENCE_TIME *), &now);
    if (data)
        VCALL(synth.buffer, 6, HRESULT(__stdcall *)(void *, REFERENCE_TIME, DWORD, DWORD, const unsigned char *),
              now, 1, len, data);
    else
        VCALL(synth.buffer, 5, HRESULT(__stdcall *)(void *, REFERENCE_TIME, DWORD, DWORD), now, 1, msg);
    VCALL(synth.port, 3, HRESULT(__stdcall *)(void *, void *), synth.buffer);
    VCALL(synth.buffer, 3, HRESULT(__stdcall *)(void *));
    synth.lock = 0;
}

static void midi_callback(UINT msg, DWORD p1)
{
    driver_callback(synth.callback, synth.flags >> 16, &synth, msg, synth.instance, p1, 0);
}

static DWORD __stdcall midi_open(void **hmo, UINT id, void *callback, DWORD instance, DWORD flags)
{
    if (synth.opened || !synth_start())
        return real_midi_open(hmo, id, callback, instance, flags);
    synth.opened = 1;
    synth.callback = (DWORD)callback;
    synth.instance = instance;
    synth.flags = flags & CALLBACK_TYPEMASK;
    *hmo = &synth;
    midi_callback(MOM_OPEN, 0);
    return 0;
}

static DWORD __stdcall midi_short(void *hmo, DWORD msg)
{
    if (hmo != &synth)
        return ((DWORD(__stdcall *)(void *, DWORD))real_midi_short)(hmo, msg);
    if (msg & 0x80)
        synth.status = msg;
    else
        msg = (msg << 8) | synth.status;
    synth_send(msg, 0, 0);
    return 0;
}

static DWORD __stdcall midi_long(void *hmo, MIDIHDR *hdr, UINT size)
{
    if (hmo != &synth)
        return ((DWORD(__stdcall *)(void *, MIDIHDR *, UINT))real_midi_long)(hmo, hdr, size);
    if (hdr->dwBufferLength)
        synth_send(0, (const unsigned char *)hdr->lpData, hdr->dwBufferLength);
    hdr->dwFlags |= MHDR_DONE;
    midi_callback(MOM_DONE, (DWORD)hdr);
    return 0;
}

static DWORD __stdcall midi_prepare(void *hmo, MIDIHDR *hdr, UINT size)
{
    if (hmo != &synth)
        return ((DWORD(__stdcall *)(void *, MIDIHDR *, UINT))real_midi_prepare)(hmo, hdr, size);
    hdr->dwFlags = (hdr->dwFlags & ~MHDR_DONE) | MHDR_PREPARED;
    return 0;
}

static DWORD __stdcall midi_unprepare(void *hmo, MIDIHDR *hdr, UINT size)
{
    if (hmo != &synth)
        return ((DWORD(__stdcall *)(void *, MIDIHDR *, UINT))real_midi_unprepare)(hmo, hdr, size);
    hdr->dwFlags &= ~MHDR_PREPARED;
    return 0;
}

static DWORD __stdcall midi_reset(void *hmo)
{
    if (hmo != &synth)
        return ((DWORD(__stdcall *)(void *))real_midi_reset)(hmo);
    for (DWORD ch = 0; ch < 16; ch++) {
        synth_send(0xB0 | ch | (123 << 8), 0, 0);
        synth_send(0xB0 | ch | (121 << 8), 0, 0);
    }
    return 0;
}

static DWORD __stdcall midi_close(void *hmo)
{
    if (hmo != &synth)
        return ((DWORD(__stdcall *)(void *))real_midi_close)(hmo);
    midi_reset(hmo);
    synth.opened = 0;
    midi_callback(MOM_CLOSE, 0);
    return 0;
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
    real_midi_open = hook("midiOutOpen", midi_open);
    real_midi_short = hook("midiOutShortMsg", midi_short);
    real_midi_long = hook("midiOutLongMsg", midi_long);
    real_midi_prepare = hook("midiOutPrepareHeader", midi_prepare);
    real_midi_unprepare = hook("midiOutUnprepareHeader", midi_unprepare);
    real_midi_reset = hook("midiOutReset", midi_reset);
    real_midi_close = hook("midiOutClose", midi_close);
    return 1;
}
