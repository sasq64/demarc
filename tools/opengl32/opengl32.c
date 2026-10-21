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

static int float_pbuffer(void *handle);
static int bind_float_pbuffer(int i);

static int __stdcall bind_tex_image(void *pbuffer, int buffer)
{
    if (float_pbuffer(pbuffer) >= 0)
        return bind_float_pbuffer(float_pbuffer(pbuffer));
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

// Xwayland's GLX has no float fbconfigs, so wine has no WGL_TYPE_RGBA_FLOAT
// format. A float pbuffer becomes a texture drawn through an FBO by contexts
// made on the DC it was created from (Plastic's "Lady Of Dark Autumn").
#define WGL_PIXEL_TYPE_ARB 0x2013
#define WGL_COLOR_BITS_ARB 0x2014
#define WGL_ALPHA_BITS_ARB 0x201B
#define WGL_DEPTH_BITS_ARB 0x2022
#define WGL_PBUFFER_LOST_ARB 0x2036
#define WGL_TYPE_RGBA_FLOAT_ARB 0x21A0
#define WGL_TEXTURE_RECTANGLE_NV 0x20A2
#define FLOAT_FORMAT 0x46000000
#define FLOAT_PBUFFERS 16
#define GL_FRAMEBUFFER 0x8D40

static struct {
    void *dc;
    int format, width, height, texture_format, target;
    unsigned texture, depth;
} pbuffers[FLOAT_PBUFFERS];

static struct {
    HGLRC context;
    unsigned pbuffer, framebuffer;
} framebuffers[64];

static int(__stdcall *wine_choose_pixel_format_arb)(void *, const int *, const float *, unsigned, int *,
                                                    unsigned *);
static void *(__stdcall *wine_create_pbuffer)(void *, int, int, int, const int *);
static int(__stdcall *wine_release_pbuffer_dc)(void *, void *);
static int(__stdcall *wine_destroy_pbuffer)(void *);
static int(__stdcall *wine_release_tex_image)(void *, int);
static HGLRC(__stdcall *wine_create_context)(void *dc);
static int(__stdcall *wine_make_current)(void *dc, HGLRC context);

static int float_pbuffer(void *handle)
{
    unsigned i = (unsigned)handle - (FLOAT_FORMAT | 0x500000);
    return i < FLOAT_PBUFFERS && pbuffers[i].dc ? (int)i : -1;
}

static int float_dc(void *dc)
{
    unsigned i = (unsigned)dc - (FLOAT_FORMAT | 0xd00000);
    return i < FLOAT_PBUFFERS && pbuffers[i].dc ? (int)i : -1;
}

static unsigned float_texture(int i)
{
    void(__stdcall * get_integerv)(unsigned, int *) = lookup(wine, "glGetIntegerv");
    void(__stdcall * gen_textures)(int, unsigned *) = lookup(wine, "glGenTextures");
    void(__stdcall * bind_texture)(unsigned, unsigned) = lookup(wine, "glBindTexture");
    void(__stdcall * tex_parameteri)(unsigned, unsigned, int) = lookup(wine, "glTexParameteri");
    void(__stdcall * tex_image)(unsigned, int, int, int, int, int, unsigned, unsigned, const void *) =
        lookup(wine, "glTexImage2D");
    if (pbuffers[i].texture)
        return pbuffers[i].texture;
    int bound = 0;
    get_integerv(0x8069 /* GL_TEXTURE_BINDING_2D */, &bound);
    gen_textures(1, &pbuffers[i].texture);
    bind_texture(0x0DE1, pbuffers[i].texture);
    tex_parameteri(0x0DE1, 0x2801 /* GL_TEXTURE_MIN_FILTER */, 0x2600 /* GL_NEAREST */);
    tex_image(0x0DE1, 0, pbuffers[i].format & 0xffff, pbuffers[i].width, pbuffers[i].height, 0,
              0x1908 /* GL_RGBA */, 0x1406 /* GL_FLOAT */, 0);
    bind_texture(0x0DE1, bound);
    return pbuffers[i].texture;
}

static unsigned float_framebuffer(HGLRC context, int i)
{
    void(__stdcall * gen_framebuffers)(int, unsigned *) = wine_get_proc_address("glGenFramebuffers");
    void(__stdcall * bind_framebuffer)(unsigned, unsigned) = wine_get_proc_address("glBindFramebuffer");
    void(__stdcall * framebuffer_texture)(unsigned, unsigned, unsigned, unsigned, int) =
        wine_get_proc_address("glFramebufferTexture2D");
    void(__stdcall * gen_renderbuffers)(int, unsigned *) = wine_get_proc_address("glGenRenderbuffers");
    void(__stdcall * bind_renderbuffer)(unsigned, unsigned) = wine_get_proc_address("glBindRenderbuffer");
    void(__stdcall * renderbuffer_storage)(unsigned, unsigned, int, int) =
        wine_get_proc_address("glRenderbufferStorage");
    void(__stdcall * framebuffer_renderbuffer)(unsigned, unsigned, unsigned, unsigned) =
        wine_get_proc_address("glFramebufferRenderbuffer");
    unsigned n = 0;
    for (; n < 64 && framebuffers[n].context; n++)
        if (framebuffers[n].context == context && framebuffers[n].pbuffer == (unsigned)i)
            return framebuffers[n].framebuffer;
    if (n == 64 || !gen_framebuffers)
        return 0;
    unsigned framebuffer = 0;
    gen_framebuffers(1, &framebuffer);
    bind_framebuffer(GL_FRAMEBUFFER, framebuffer);
    framebuffer_texture(GL_FRAMEBUFFER, 0x8CE0 /* GL_COLOR_ATTACHMENT0 */, 0x0DE1, float_texture(i), 0);
    if (pbuffers[i].format & 0xff0000) {
        if (!pbuffers[i].depth) {
            gen_renderbuffers(1, &pbuffers[i].depth);
            bind_renderbuffer(0x8D41 /* GL_RENDERBUFFER */, pbuffers[i].depth);
            renderbuffer_storage(0x8D41, 0x81A6 /* GL_DEPTH_COMPONENT24 */, pbuffers[i].width,
                                 pbuffers[i].height);
        }
        framebuffer_renderbuffer(GL_FRAMEBUFFER, 0x8D00 /* GL_DEPTH_ATTACHMENT */, 0x8D41, pbuffers[i].depth);
    }
    framebuffers[n].context = context;
    framebuffers[n].pbuffer = i;
    framebuffers[n].framebuffer = framebuffer;
    return framebuffer;
}

static int __stdcall choose_pixel_format_arb(void *dc, const int *ints, const float *floats, unsigned max,
                                             int *formats, unsigned *count)
{
    int is_float = 0, bits = 0, depth = 0;
    for (const int *a = ints; a && a[0]; a += 2) {
        if (a[0] == WGL_PIXEL_TYPE_ARB)
            is_float = a[1] == WGL_TYPE_RGBA_FLOAT_ARB;
        else if (a[0] == WGL_COLOR_BITS_ARB || a[0] == WGL_ALPHA_BITS_ARB)
            bits += a[1];
        else if (a[0] == WGL_DEPTH_BITS_ARB)
            depth = a[1];
    }
    if (!is_float)
        return wine_choose_pixel_format_arb(dc, ints, floats, max, formats, count);
    if (max)
        formats[0] = FLOAT_FORMAT | (depth ? 0x10000 : 0) | (bits > 64 ? 0x8814 /* GL_RGBA32F */ : 0x881A);
    *count = max ? 1 : 0;
    return 1;
}

static void *__stdcall create_pbuffer(void *dc, int format, int width, int height, const int *attribs)
{
    if ((format & 0xff000000) != FLOAT_FORMAT)
        return wine_create_pbuffer(dc, format, width, height, attribs);
    int i = 0;
    while (i < FLOAT_PBUFFERS && pbuffers[i].dc)
        i++;
    if (i == FLOAT_PBUFFERS)
        return 0;
    pbuffers[i].dc = dc;
    pbuffers[i].format = format;
    pbuffers[i].width = width;
    pbuffers[i].height = height;
    pbuffers[i].texture_format = pbuffers[i].target = 0;
    pbuffers[i].texture = pbuffers[i].depth = 0;
    for (const int *a = attribs; a && a[0]; a += 2) {
        if (a[0] == WGL_TEXTURE_FORMAT_ARB)
            pbuffers[i].texture_format = a[1];
        else if (a[0] == WGL_TEXTURE_TARGET_ARB)
            pbuffers[i].target = a[1];
    }
    return (void *)(FLOAT_FORMAT | 0x500000 | i);
}

static void *__stdcall pbuffer_dc(void *pbuffer)
{
    int i = float_pbuffer(pbuffer);
    return i < 0 ? get_pbuffer_dc(pbuffer) : (void *)(FLOAT_FORMAT | 0xd00000 | i);
}

static int __stdcall release_pbuffer_dc(void *pbuffer, void *dc)
{
    return float_pbuffer(pbuffer) < 0 ? wine_release_pbuffer_dc(pbuffer, dc) : 1;
}

static int __stdcall destroy_pbuffer(void *pbuffer)
{
    int i = float_pbuffer(pbuffer);
    if (i < 0)
        return wine_destroy_pbuffer(pbuffer);
    pbuffers[i].dc = 0;
    for (unsigned n = 0; n < 64; n++)
        if (framebuffers[n].context && framebuffers[n].pbuffer == (unsigned)i)
            framebuffers[n].pbuffer = ~0u;
    return 1;
}

static int __stdcall query_float_pbuffer(void *pbuffer, int attribute, int *value)
{
    int i = float_pbuffer(pbuffer);
    if (i < 0)
        return query_pbuffer(pbuffer, attribute, value);
    switch (attribute) {
    case WGL_PBUFFER_WIDTH_ARB: *value = pbuffers[i].width; return 1;
    case WGL_PBUFFER_HEIGHT_ARB: *value = pbuffers[i].height; return 1;
    case WGL_PBUFFER_LOST_ARB: *value = 0; return 1;
    case WGL_TEXTURE_FORMAT_ARB: *value = pbuffers[i].texture_format; return 1;
    case WGL_TEXTURE_TARGET_ARB: *value = pbuffers[i].target; return 1;
    }
    return 0;
}

// Gives the texture bound by the caller a copy of the pbuffer's.
static int bind_float_pbuffer(int i)
{
    void(__stdcall * get_integerv)(unsigned, int *) = lookup(wine, "glGetIntegerv");
    void(__stdcall * get_level_parameteriv)(unsigned, int, unsigned, int *) =
        lookup(wine, "glGetTexLevelParameteriv");
    void(__stdcall * tex_image)(unsigned, int, int, int, int, int, unsigned, unsigned, const void *) =
        lookup(wine, "glTexImage2D");
    void(__stdcall * copy_image)(unsigned, unsigned, int, int, int, int, unsigned, unsigned, int, int, int,
                                 int, int, int, int) = wine_get_proc_address("glCopyImageSubData");
    int rectangle = pbuffers[i].target == WGL_TEXTURE_RECTANGLE_NV;
    unsigned target = rectangle ? 0x84F5 /* GL_TEXTURE_RECTANGLE */ : 0x0DE1;
    int texture = 0, width = 0, height = 0, format = 0;
    get_integerv(rectangle ? 0x84F6 : 0x8069, &texture);
    if (!texture || !copy_image)
        return 0;
    get_level_parameteriv(target, 0, 0x1000 /* GL_TEXTURE_WIDTH */, &width);
    get_level_parameteriv(target, 0, 0x1001 /* GL_TEXTURE_HEIGHT */, &height);
    get_level_parameteriv(target, 0, 0x1003 /* GL_TEXTURE_INTERNAL_FORMAT */, &format);
    if (width != pbuffers[i].width || height != pbuffers[i].height || format != (pbuffers[i].format & 0xffff))
        tex_image(target, 0, pbuffers[i].format & 0xffff, pbuffers[i].width, pbuffers[i].height, 0, 0x1908,
                  0x1406, 0);
    copy_image(float_texture(i), 0x0DE1, 0, 0, 0, 0, texture, target, 0, 0, 0, 0, pbuffers[i].width,
               pbuffers[i].height, 1);
    return 1;
}

static int __stdcall release_tex_image(void *pbuffer, int buffer)
{
    return float_pbuffer(pbuffer) < 0 ? wine_release_tex_image(pbuffer, buffer) : 1;
}

static HGLRC __stdcall create_context(void *dc)
{
    int i = float_dc(dc);
    return wine_create_context(i < 0 ? dc : pbuffers[i].dc);
}

static int __stdcall make_current(void *dc, HGLRC context)
{
    int i = float_dc(dc);
    int ret = wine_make_current(i < 0 ? dc : pbuffers[i].dc, context);
    unsigned n = 0;
    while (n < 64 && framebuffers[n].context && framebuffers[n].context != context)
        n++;
    if (ret && context && (i >= 0 || (n < 64 && framebuffers[n].context))) {
        void(__stdcall * bind_framebuffer)(unsigned, unsigned) = wine_get_proc_address("glBindFramebuffer");
        unsigned framebuffer = i < 0 ? 0 : float_framebuffer(context, i);
        if (bind_framebuffer)
            bind_framebuffer(GL_FRAMEBUFFER, framebuffer);
    }
    return ret;
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
    if (address && equal(name, "wglChoosePixelFormatARB")) {
        wine_choose_pixel_format_arb = address;
        return choose_pixel_format_arb;
    }
    if (address && equal(name, "wglCreatePbufferARB")) {
        wine_create_pbuffer = address;
        return create_pbuffer;
    }
    if (address && equal(name, "wglGetPbufferDCARB")) {
        get_pbuffer_dc = address;
        return pbuffer_dc;
    }
    if (address && equal(name, "wglReleasePbufferDCARB")) {
        wine_release_pbuffer_dc = address;
        return release_pbuffer_dc;
    }
    if (address && equal(name, "wglDestroyPbufferARB")) {
        wine_destroy_pbuffer = address;
        return destroy_pbuffer;
    }
    if (address && equal(name, "wglQueryPbufferARB")) {
        query_pbuffer = address;
        return query_float_pbuffer;
    }
    if (address && equal(name, "wglReleaseTexImageARB")) {
        wine_release_tex_image = address;
        return release_tex_image;
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

// No GLX config has an accumulation buffer on NVIDIA's Xwayland, so glAccum
// fails and Fizzer's "The Real Party Is In Your Pocket" shows one raw sample.
// Without accum bits the buffer is an RGBA32F texture drawn into through an FBO.
int _fltused;

static void(__stdcall *wine_accum)(unsigned op, float value);
static void(__stdcall *wine_clear)(unsigned mask);
static void(__stdcall *wine_clear_accum)(float r, float g, float b, float a);

static struct {
    HGLRC context;
    int width, height;
    unsigned accum, color, framebuffer, program;
    int scale, add;
    float clear[4];
} accums[8];

static const char *const accum_vs = "#version 130\nvoid main(){gl_Position=gl_Vertex;}";
static const char *const accum_fs = "#version 130\nuniform sampler2D t;uniform vec4 s,a;"
                                    "void main(){gl_FragColor=texelFetch(t,ivec2(gl_FragCoord.xy),0)*s+a;}";

static unsigned accum_texture(int width, int height, int format, unsigned type)
{
    void(__stdcall * gen_textures)(int, unsigned *) = lookup(wine, "glGenTextures");
    void(__stdcall * bind_texture)(unsigned, unsigned) = lookup(wine, "glBindTexture");
    void(__stdcall * tex_parameteri)(unsigned, unsigned, int) = lookup(wine, "glTexParameteri");
    void(__stdcall * tex_image)(unsigned, int, int, int, int, int, unsigned, unsigned, const void *) =
        lookup(wine, "glTexImage2D");
    unsigned texture = 0;
    gen_textures(1, &texture);
    bind_texture(0x0DE1, texture);
    tex_parameteri(0x0DE1, 0x2801 /* GL_TEXTURE_MIN_FILTER */, 0x2600 /* GL_NEAREST */);
    tex_image(0x0DE1, 0, format, width, height, 0, 0x1908 /* GL_RGBA */, type, 0);
    return texture;
}

static unsigned accum_shader(unsigned type, const char *source)
{
    unsigned(__stdcall * create_shader)(unsigned) = wine_get_proc_address("glCreateShader");
    void(__stdcall * shader_source)(unsigned, int, const char *const *, const int *) =
        wine_get_proc_address("glShaderSource");
    void(__stdcall * compile_shader)(unsigned) = wine_get_proc_address("glCompileShader");
    unsigned shader = create_shader(type);
    shader_source(shader, 1, &source, 0);
    compile_shader(shader);
    return shader;
}

// The emulated accumulation buffer of the current context, or -1 when it has
// a real one. Made with the viewport's size on first use.
static int emulated_accum(void)
{
    HGLRC(__stdcall * current)(void) = lookup(wine, "wglGetCurrentContext");
    void(__stdcall * get_integerv)(unsigned, int *) = lookup(wine, "glGetIntegerv");
    HGLRC context = current();
    unsigned i = 0;
    for (; i < 8 && accums[i].context; i++)
        if (accums[i].context == context)
            return accums[i].framebuffer ? (int)i : -1;
    if (!context || i == 8)
        return -1;
    accums[i].context = context;
    int bits = 0;
    get_integerv(0x0D58 /* GL_ACCUM_RED_BITS */, &bits);
    void(__stdcall * gen_framebuffers)(int, unsigned *) = wine_get_proc_address("glGenFramebuffers");
    if (bits || !gen_framebuffers || !wine_get_proc_address("glCreateProgram"))
        return -1;
    void(__stdcall * bind_framebuffer)(unsigned, unsigned) = wine_get_proc_address("glBindFramebuffer");
    void(__stdcall * framebuffer_texture)(unsigned, unsigned, unsigned, unsigned, int) =
        wine_get_proc_address("glFramebufferTexture2D");
    unsigned(__stdcall * create_program)(void) = wine_get_proc_address("glCreateProgram");
    void(__stdcall * attach_shader)(unsigned, unsigned) = wine_get_proc_address("glAttachShader");
    void(__stdcall * link_program)(unsigned) = wine_get_proc_address("glLinkProgram");
    int(__stdcall * uniform_location)(unsigned, const char *) = wine_get_proc_address("glGetUniformLocation");
    int viewport[4], texture = 0, framebuffer = 0;
    get_integerv(0x0BA2 /* GL_VIEWPORT */, viewport);
    get_integerv(0x8069 /* GL_TEXTURE_BINDING_2D */, &texture);
    get_integerv(0x8CA6 /* GL_DRAW_FRAMEBUFFER_BINDING */, &framebuffer);
    accums[i].width = viewport[0] + viewport[2];
    accums[i].height = viewport[1] + viewport[3];
    accums[i].accum = accum_texture(accums[i].width, accums[i].height, 0x8814 /* GL_RGBA32F */, 0x1406);
    accums[i].color = accum_texture(accums[i].width, accums[i].height, 0x8058 /* GL_RGBA8 */, 0x1401);
    ((void(__stdcall *)(unsigned, unsigned))lookup(wine, "glBindTexture"))(0x0DE1, texture);
    gen_framebuffers(1, &accums[i].framebuffer);
    bind_framebuffer(0x8CA9 /* GL_DRAW_FRAMEBUFFER */, accums[i].framebuffer);
    framebuffer_texture(0x8CA9, 0x8CE0 /* GL_COLOR_ATTACHMENT0 */, 0x0DE1, accums[i].accum, 0);
    bind_framebuffer(0x8CA9, framebuffer);
    unsigned program = create_program();
    attach_shader(program, accum_shader(0x8B31 /* GL_VERTEX_SHADER */, accum_vs));
    attach_shader(program, accum_shader(0x8B30 /* GL_FRAGMENT_SHADER */, accum_fs));
    link_program(program);
    accums[i].program = program;
    accums[i].scale = uniform_location(program, "s");
    accums[i].add = uniform_location(program, "a");
    __wine_dbg_output("opengl32: emulating the accumulation buffer\n");
    return (int)i;
}

static void clear_emulated_accum(int i)
{
    void(__stdcall * get_integerv)(unsigned, int *) = lookup(wine, "glGetIntegerv");
    void(__stdcall * get_floatv)(unsigned, float *) = lookup(wine, "glGetFloatv");
    void(__stdcall * clear_color)(float, float, float, float) = lookup(wine, "glClearColor");
    void(__stdcall * bind_framebuffer)(unsigned, unsigned) = wine_get_proc_address("glBindFramebuffer");
    int framebuffer = 0;
    float color[4];
    get_integerv(0x8CA6 /* GL_DRAW_FRAMEBUFFER_BINDING */, &framebuffer);
    get_floatv(0x0C22 /* GL_COLOR_CLEAR_VALUE */, color);
    bind_framebuffer(0x8CA9, accums[i].framebuffer);
    clear_color(accums[i].clear[0], accums[i].clear[1], accums[i].clear[2], accums[i].clear[3]);
    wine_clear(0x4000 /* GL_COLOR_BUFFER_BIT */);
    clear_color(color[0], color[1], color[2], color[3]);
    bind_framebuffer(0x8CA9, framebuffer);
}

static void __stdcall clear(unsigned mask)
{
    int i;
    if ((mask & 0x200 /* GL_ACCUM_BUFFER_BIT */) && (i = emulated_accum()) >= 0) {
        clear_emulated_accum(i);
        mask &= ~0x200u;
        if (!mask)
            return;
    }
    wine_clear(mask);
}

static void __stdcall clear_accum(float r, float g, float b, float a)
{
    int i = emulated_accum();
    if (i < 0) {
        wine_clear_accum(r, g, b, a);
        return;
    }
    accums[i].clear[0] = r;
    accums[i].clear[1] = g;
    accums[i].clear[2] = b;
    accums[i].clear[3] = a;
}

static void __stdcall accum(unsigned op, float value)
{
    int i = emulated_accum();
    if (i < 0) {
        wine_accum(op, value);
        return;
    }
    void(__stdcall * get_integerv)(unsigned, int *) = lookup(wine, "glGetIntegerv");
    unsigned char(__stdcall * is_enabled)(unsigned) = lookup(wine, "glIsEnabled");
    void(__stdcall * enable)(unsigned) = lookup(wine, "glEnable");
    void(__stdcall * disable)(unsigned) = lookup(wine, "glDisable");
    void(__stdcall * bind_texture)(unsigned, unsigned) = lookup(wine, "glBindTexture");
    void(__stdcall * viewport)(int, int, int, int) = lookup(wine, "glViewport");
    void(__stdcall * blend_func)(unsigned, unsigned) = lookup(wine, "glBlendFunc");
    void(__stdcall * copy_tex)(unsigned, int, int, int, int, int, int, int) = lookup(wine, "glCopyTexSubImage2D");
    void(__stdcall * rect)(float, float, float, float) = lookup(wine, "glRectf");
    void(__stdcall * bind_framebuffer)(unsigned, unsigned) = wine_get_proc_address("glBindFramebuffer");
    void(__stdcall * use_program)(unsigned) = wine_get_proc_address("glUseProgram");
    void(__stdcall * active_texture)(unsigned) = wine_get_proc_address("glActiveTexture");
    void(__stdcall * uniform4f)(int, float, float, float, float) = wine_get_proc_address("glUniform4f");
    void(__stdcall * blend_func_separate)(unsigned, unsigned, unsigned, unsigned) =
        wine_get_proc_address("glBlendFuncSeparate");
    void(__stdcall * blend_equation_separate)(unsigned, unsigned) =
        wine_get_proc_address("glBlendEquationSeparate");

    static const unsigned caps[] = {0x0BE2 /* GL_BLEND */, 0x0B71 /* GL_DEPTH_TEST */, 0x0B44 /* GL_CULL_FACE */,
                                    0x0B90 /* GL_STENCIL_TEST */, 0x0BC0 /* GL_ALPHA_TEST */};
    unsigned char enabled[5];
    int program, active, texture, framebuffer, view[4], blend[4], equation[2];
    for (unsigned c = 0; c < 5; c++)
        enabled[c] = is_enabled(caps[c]);
    get_integerv(0x8B8D /* GL_CURRENT_PROGRAM */, &program);
    get_integerv(0x84E0 /* GL_ACTIVE_TEXTURE */, &active);
    active_texture(0x84C0 /* GL_TEXTURE0 */);
    get_integerv(0x8069 /* GL_TEXTURE_BINDING_2D */, &texture);
    get_integerv(0x8CA6 /* GL_DRAW_FRAMEBUFFER_BINDING */, &framebuffer);
    get_integerv(0x0BA2 /* GL_VIEWPORT */, view);
    get_integerv(0x80C9 /* GL_BLEND_SRC_RGB */, &blend[0]);
    get_integerv(0x80C8 /* GL_BLEND_DST_RGB */, &blend[1]);
    get_integerv(0x80CB /* GL_BLEND_SRC_ALPHA */, &blend[2]);
    get_integerv(0x80CA /* GL_BLEND_DST_ALPHA */, &blend[3]);
    get_integerv(0x8009 /* GL_BLEND_EQUATION_RGB */, &equation[0]);
    get_integerv(0x883D /* GL_BLEND_EQUATION_ALPHA */, &equation[1]);
    for (unsigned c = 1; c < 5; c++)
        disable(caps[c]);

    use_program(accums[i].program);
    viewport(0, 0, accums[i].width, accums[i].height);
    blend_equation_separate(0x8006 /* GL_FUNC_ADD */, 0x8006);
    float s = value, a = 0;
    if (op == 0x0100 /* GL_ACCUM */ || op == 0x0101 /* GL_LOAD */) {
        bind_texture(0x0DE1, accums[i].color);
        copy_tex(0x0DE1, 0, 0, 0, 0, 0, accums[i].width, accums[i].height);
        bind_framebuffer(0x8CA9, accums[i].framebuffer);
        blend_func(1 /* GL_ONE */, op == 0x0100 ? 1 : 0);
    } else if (op == 0x0102 /* GL_RETURN */) {
        bind_texture(0x0DE1, accums[i].accum);
        blend_func(1, 0);
    } else {
        bind_framebuffer(0x8CA9, accums[i].framebuffer);
        s = 0;
        a = value;
        // GL_ADD: accum + value; GL_MULT: accum * value.
        if (op == 0x0103)
            blend_func(0x0306 /* GL_DST_COLOR */, 0);
        else
            blend_func(1, 1);
    }
    enable(0x0BE2);
    uniform4f(accums[i].scale, s, s, s, s);
    uniform4f(accums[i].add, a, a, a, a);
    rect(-1, -1, 1, 1);

    bind_framebuffer(0x8CA9, framebuffer);
    bind_texture(0x0DE1, texture);
    active_texture(active);
    use_program(program);
    viewport(view[0], view[1], view[2], view[3]);
    blend_func_separate(blend[0], blend[1], blend[2], blend[3]);
    blend_equation_separate(equation[0], equation[1]);
    for (unsigned c = 0; c < 5; c++)
        (enabled[c] ? enable : disable)(caps[c]);
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
            } else if (address && equal(names[i], "wglCreateContext")) {
                wine_create_context = address;
                address = create_context;
            } else if (address && equal(names[i], "wglMakeCurrent")) {
                wine_make_current = address;
                address = make_current;
            } else if (address && equal(names[i], "wglSwapBuffers")) {
                wine_swap_buffers = address;
                address = swap_buffers;
            } else if (address && equal(names[i], "glAccum")) {
                wine_accum = address;
                address = accum;
            } else if (address && equal(names[i], "glClear")) {
                wine_clear = address;
                address = clear;
            } else if (address && equal(names[i], "glClearAccum")) {
                wine_clear_accum = address;
                address = clear_accum;
            }
        } else {
            LdrGetProcedureAddress(wine, 0, ordinals[i], &address);
        }
        if (address)
            slots[i] = address;
    }
    return 1;
}
