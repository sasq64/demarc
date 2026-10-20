// opengl32.dll with the export table of wine's, every export a jump into
// wine's own opengl32, wglGetProcAddress also finding the GL 1.1 exports.
// See README.md.

typedef unsigned short WCHAR;
typedef unsigned short USHORT;
typedef unsigned long ULONG;
typedef long NTSTATUS;
typedef void *HMODULE;

#include "glsl.h"

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
__declspec(dllimport) void *__stdcall RtlAllocateHeap(void *heap, ULONG flags, ULONG size);
__declspec(dllimport) int __stdcall RtlFreeHeap(void *heap, ULONG flags, void *p);
__declspec(dllimport) int __cdecl __wine_dbg_output(const char *text);

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

// wine copies a render-texture pbuffer with its internal context, whose pixel
// format is not the pbuffer's; NVIDIA's GLX fails that with a fatal BadMatch.
// Copy with a context made on the pbuffer's own DC instead (Plastic's "Your
// Fingers So Gently On My Skin").
typedef void *HGLRC;
static int(__stdcall *wine_bind_tex_image)(void *pbuffer, int buffer);
static void *(__stdcall *get_pbuffer_dc)(void *pbuffer);
static int(__stdcall *query_pbuffer)(void *pbuffer, int attribute, int *value);

#define WGL_PBUFFER_WIDTH_ARB 0x2034
#define WGL_PBUFFER_HEIGHT_ARB 0x2035
#define WGL_TEXTURE_FORMAT_ARB 0x2072
#define WGL_TEXTURE_TARGET_ARB 0x2073
#define WGL_TEXTURE_RGB_ARB 0x2075
#define WGL_TEXTURE_2D_ARB 0x207A
#define WGL_FRONT_LEFT_ARB 0x2083
#define WGL_BACK_LEFT_ARB 0x2085

static struct {
    void *dc;
    HGLRC context;
} copiers[16];

static HGLRC copier_for(void *dc, HGLRC share)
{
    HGLRC(__stdcall * create)(void *) = lookup(wine, "wglCreateContext");
    int(__stdcall * share_lists)(HGLRC, HGLRC) = lookup(wine, "wglShareLists");
    unsigned i = 0;
    for (; i < 16 && copiers[i].dc; i++)
        if (copiers[i].dc == dc)
            return copiers[i].context;
    if (i == 16)
        return 0;
    HGLRC context = create(dc);
    if (!context || !share_lists(share, context))
        return 0;
    copiers[i].dc = dc;
    copiers[i].context = context;
    return context;
}

static int __stdcall bind_tex_image(void *pbuffer, int buffer)
{
    HGLRC(__stdcall * current_context)(void) = lookup(wine, "wglGetCurrentContext");
    void *(__stdcall * current_dc)(void) = lookup(wine, "wglGetCurrentDC");
    int(__stdcall * make_current)(void *, HGLRC) = lookup(wine, "wglMakeCurrent");
    void(__stdcall * get_integerv)(unsigned, int *) = lookup(wine, "glGetIntegerv");
    void(__stdcall * bind_texture)(unsigned, unsigned) = lookup(wine, "glBindTexture");
    void(__stdcall * read_buffer)(unsigned) = lookup(wine, "glReadBuffer");
    void(__stdcall * copy_tex_image)(unsigned, int, unsigned, int, int, int, int, int) =
        lookup(wine, "glCopyTexImage2D");
    void(__stdcall * flush)(void) = lookup(wine, "glFlush");

    int target = 0, format = 0, width = 0, height = 0, texture = 0;
    void *dc = get_pbuffer_dc ? get_pbuffer_dc(pbuffer) : 0;
    HGLRC context = current_context();
    void *context_dc = current_dc();
    if (!dc || !query_pbuffer || (buffer != WGL_FRONT_LEFT_ARB && buffer != WGL_BACK_LEFT_ARB) ||
        !query_pbuffer(pbuffer, WGL_TEXTURE_TARGET_ARB, &target) || target != WGL_TEXTURE_2D_ARB ||
        !query_pbuffer(pbuffer, WGL_TEXTURE_FORMAT_ARB, &format) ||
        !query_pbuffer(pbuffer, WGL_PBUFFER_WIDTH_ARB, &width) ||
        !query_pbuffer(pbuffer, WGL_PBUFFER_HEIGHT_ARB, &height))
        return wine_bind_tex_image(pbuffer, buffer);
    HGLRC copier = copier_for(dc, context);
    if (!copier)
        return wine_bind_tex_image(pbuffer, buffer);

    get_integerv(0x8069 /* GL_TEXTURE_BINDING_2D */, &texture);
    make_current(dc, copier);
    bind_texture(0x0DE1 /* GL_TEXTURE_2D */, texture);
    read_buffer(buffer == WGL_FRONT_LEFT_ARB ? 0x0404 /* GL_FRONT */ : 0x0405 /* GL_BACK */);
    copy_tex_image(0x0DE1, 0, format == WGL_TEXTURE_RGB_ARB ? 0x1907 : 0x1908, 0, 0, width,
                   height, 0);
    flush();
    make_current(context_dc, context);
    return 1;
}

// GLSL patches from the override, applied as shaders are handed to GL.
static char *peb(void)
{
    char *p;
    __asm__("movl %%fs:0x30, %0" : "=r"(p));
    return p;
}

void *glsl_alloc(unsigned size) { return RtlAllocateHeap(*(void **)(peb() + 0x18), 0, size); }

void glsl_release(void *p)
{
    if (p)
        RtlFreeHeap(*(void **)(peb() + 0x18), 0, p);
}

// Wine's stderr, whatever WINEDEBUG says; the demo's own std handles are unset.
void glsl_log(const char *line) { __wine_dbg_output(line); }

static void load_glsl_patches(void)
{
    static int loaded;
    if (loaded)
        return;
    loaded = 1;
    WCHAR var[] = u"DEMARC_GLSL_PATCHES";
    UNICODE_STRING name = {sizeof(var) - 2, sizeof(var), var};
    UNICODE_STRING value = {0, 0, 0};
    RtlQueryEnvironmentVariable_U(0, &name, &value);
    if (!value.Length)
        return;
    value.MaximumLength = value.Length + 2;
    value.Buffer = glsl_alloc(value.MaximumLength);
    char *config = glsl_alloc(value.Length / 2 + 1);
    if (value.Buffer && config && !RtlQueryEnvironmentVariable_U(0, &name, &value)) {
        unsigned n = value.Length / 2;
        for (unsigned i = 0; i < n; i++)
            config[i] = value.Buffer[i] < 128 ? value.Buffer[i] : '?';
        config[n] = 0;
        glsl_configure(config);
    }
    glsl_release(value.Buffer);
    glsl_release(config);
}

static void(__stdcall *wine_shader_source)(unsigned, int, const char *const *, const int *);
static void(__stdcall *wine_shader_source_arb)(unsigned, int, const char *const *, const int *);
static unsigned(__stdcall *wine_create_shader_programv)(unsigned, int, const char *const *);

static void __stdcall shader_source(unsigned shader, int count, const char *const *strings,
                                    const int *lengths)
{
    load_glsl_patches();
    const char *patched = glsl_patch(count, strings, lengths);
    if (patched)
        wine_shader_source(shader, 1, &patched, 0);
    else
        wine_shader_source(shader, count, strings, lengths);
    glsl_release((void *)patched);
}

static void __stdcall shader_source_arb(unsigned shader, int count, const char *const *strings,
                                        const int *lengths)
{
    load_glsl_patches();
    const char *patched = glsl_patch(count, strings, lengths);
    if (patched)
        wine_shader_source_arb(shader, 1, &patched, 0);
    else
        wine_shader_source_arb(shader, count, strings, lengths);
    glsl_release((void *)patched);
}

static unsigned __stdcall create_shader_programv(unsigned type, int count, const char *const *strings)
{
    load_glsl_patches();
    const char *patched = glsl_patch(count, strings, 0);
    unsigned program = patched ? wine_create_shader_programv(type, 1, &patched)
                               : wine_create_shader_programv(type, count, strings);
    glsl_release((void *)patched);
    return program;
}

// NVIDIA's driver hands out the GL 1.1 functions too, and Hexer relies on it.
static void *__stdcall get_proc_address(const char *name)
{
    void *address = wine_get_proc_address(name);
    if (address && equal(name, "glShaderSource")) {
        wine_shader_source = address;
        return shader_source;
    }
    if (address && equal(name, "glShaderSourceARB")) {
        wine_shader_source_arb = address;
        return shader_source_arb;
    }
    if (address && equal(name, "glCreateShaderProgramv")) {
        wine_create_shader_programv = address;
        return create_shader_programv;
    }
    if (address && equal(name, "wglBindTexImageARB")) {
        wine_bind_tex_image = address;
        get_pbuffer_dc = wine_get_proc_address("wglGetPbufferDCARB");
        query_pbuffer = wine_get_proc_address("wglQueryPbufferARB");
        return bind_tex_image;
    }
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

// The Explorer hard-codes format 8, double-buffered on Windows but not in
// wine's list, and front-buffer drawing never reaches the session.
static int(__stdcall *wine_set_pixel_format)(void *hdc, int format, const void *pfd);

static int __stdcall set_pixel_format(void *hdc, int format, const void *pfd)
{
    int(__stdcall * describe)(void *, int, unsigned, unsigned char *) = lookup(wine, "wglDescribePixelFormat");
    unsigned char desc[40];
    if (describe && describe(hdc, format, sizeof(desc), desc) && !(desc[4] & 1)) {
        desc[4] |= 1;
        int double_buffered = wine_choose_pixel_format(hdc, desc);
        if (double_buffered && describe(hdc, double_buffered, sizeof(desc), desc) && (desc[4] & 1))
            format = double_buffered;
    }
    return wine_set_pixel_format(hdc, format, pfd);
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
            } else if (address && equal(names[i], "wglSetPixelFormat")) {
                wine_set_pixel_format = address;
                address = set_pixel_format;
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
