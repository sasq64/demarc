// dx8vb.dll with the export table of wine's, the VB_D3DX* math functions
// implemented, everything else a jump into wine's own dx8vb. See README.md.

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

int _fltused;

// The functions d3dx9 only has as inlines in d3dx9math.inl.

typedef struct { float x, y; } V2;
typedef struct { float x, y, z; } V3;
typedef struct { float x, y, z, w; } V4;
typedef struct { float m[16]; } M;

#define API __stdcall

static float API v2_length_sq(const V2 *v) { return v->x * v->x + v->y * v->y; }
static float API v2_length(const V2 *v) { return __builtin_sqrtf(v2_length_sq(v)); }
static float API v2_dot(const V2 *a, const V2 *b) { return a->x * b->x + a->y * b->y; }
static float API v2_ccw(const V2 *a, const V2 *b) { return a->x * b->y - a->y * b->x; }
static void API v2_add(V2 *o, const V2 *a, const V2 *b) { o->x = a->x + b->x; o->y = a->y + b->y; }
static void API v2_sub(V2 *o, const V2 *a, const V2 *b) { o->x = a->x - b->x; o->y = a->y - b->y; }
static void API v2_min(V2 *o, const V2 *a, const V2 *b)
{
    o->x = a->x < b->x ? a->x : b->x;
    o->y = a->y < b->y ? a->y : b->y;
}
static void API v2_max(V2 *o, const V2 *a, const V2 *b)
{
    o->x = a->x > b->x ? a->x : b->x;
    o->y = a->y > b->y ? a->y : b->y;
}
static void API v2_scale(V2 *o, const V2 *a, float s) { o->x = a->x * s; o->y = a->y * s; }
static void API v2_lerp(V2 *o, const V2 *a, const V2 *b, float s)
{
    o->x = a->x + s * (b->x - a->x);
    o->y = a->y + s * (b->y - a->y);
}

static float API v3_length_sq(const V3 *v) { return v->x * v->x + v->y * v->y + v->z * v->z; }
static float API v3_length(const V3 *v) { return __builtin_sqrtf(v3_length_sq(v)); }
static float API v3_dot(const V3 *a, const V3 *b) { return a->x * b->x + a->y * b->y + a->z * b->z; }
static void API v3_cross(V3 *o, const V3 *a, const V3 *b)
{
    V3 r = {a->y * b->z - a->z * b->y, a->z * b->x - a->x * b->z, a->x * b->y - a->y * b->x};
    o->x = r.x; o->y = r.y; o->z = r.z;
}
static void API v3_add(V3 *o, const V3 *a, const V3 *b)
{
    o->x = a->x + b->x; o->y = a->y + b->y; o->z = a->z + b->z;
}
static void API v3_sub(V3 *o, const V3 *a, const V3 *b)
{
    o->x = a->x - b->x; o->y = a->y - b->y; o->z = a->z - b->z;
}
static void API v3_min(V3 *o, const V3 *a, const V3 *b)
{
    o->x = a->x < b->x ? a->x : b->x;
    o->y = a->y < b->y ? a->y : b->y;
    o->z = a->z < b->z ? a->z : b->z;
}
static void API v3_max(V3 *o, const V3 *a, const V3 *b)
{
    o->x = a->x > b->x ? a->x : b->x;
    o->y = a->y > b->y ? a->y : b->y;
    o->z = a->z > b->z ? a->z : b->z;
}
static void API v3_scale(V3 *o, const V3 *a, float s) { o->x = a->x * s; o->y = a->y * s; o->z = a->z * s; }
static void API v3_lerp(V3 *o, const V3 *a, const V3 *b, float s)
{
    o->x = a->x + s * (b->x - a->x);
    o->y = a->y + s * (b->y - a->y);
    o->z = a->z + s * (b->z - a->z);
}

static float API v4_dot(const V4 *a, const V4 *b)
{
    return a->x * b->x + a->y * b->y + a->z * b->z + a->w * b->w;
}
static float API v4_length_sq(const V4 *v) { return v4_dot(v, v); }
static float API v4_length(const V4 *v) { return __builtin_sqrtf(v4_dot(v, v)); }
static void API v4_add(V4 *o, const V4 *a, const V4 *b)
{
    o->x = a->x + b->x; o->y = a->y + b->y; o->z = a->z + b->z; o->w = a->w + b->w;
}
static void API v4_sub(V4 *o, const V4 *a, const V4 *b)
{
    o->x = a->x - b->x; o->y = a->y - b->y; o->z = a->z - b->z; o->w = a->w - b->w;
}
static void API v4_min(V4 *o, const V4 *a, const V4 *b)
{
    o->x = a->x < b->x ? a->x : b->x;
    o->y = a->y < b->y ? a->y : b->y;
    o->z = a->z < b->z ? a->z : b->z;
    o->w = a->w < b->w ? a->w : b->w;
}
static void API v4_max(V4 *o, const V4 *a, const V4 *b)
{
    o->x = a->x > b->x ? a->x : b->x;
    o->y = a->y > b->y ? a->y : b->y;
    o->z = a->z > b->z ? a->z : b->z;
    o->w = a->w > b->w ? a->w : b->w;
}
static void API v4_scale(V4 *o, const V4 *a, float s)
{
    o->x = a->x * s; o->y = a->y * s; o->z = a->z * s; o->w = a->w * s;
}
static void API v4_lerp(V4 *o, const V4 *a, const V4 *b, float s)
{
    o->x = a->x + s * (b->x - a->x);
    o->y = a->y + s * (b->y - a->y);
    o->z = a->z + s * (b->z - a->z);
    o->w = a->w + s * (b->w - a->w);
}

static void API m_identity(M *o)
{
    for (int i = 0; i < 16; i++)
        o->m[i] = i % 5 ? 0.0f : 1.0f;
}
static int API m_is_identity(const M *m)
{
    for (int i = 0; i < 16; i++)
        if (m->m[i] != (i % 5 ? 0.0f : 1.0f))
            return 0;
    return 1;
}

static void API q_identity(V4 *o) { o->x = 0.0f; o->y = 0.0f; o->z = 0.0f; o->w = 1.0f; }
static int API q_is_identity(const V4 *q) { return q->x == 0.0f && q->y == 0.0f && q->z == 0.0f && q->w == 1.0f; }
static void API q_conjugate(V4 *o, const V4 *q) { o->x = -q->x; o->y = -q->y; o->z = -q->z; o->w = q->w; }

static float API p_dot_coord(const V4 *p, const V3 *v) { return p->x * v->x + p->y * v->y + p->z * v->z + p->w; }
static float API p_dot_normal(const V4 *p, const V3 *v) { return p->x * v->x + p->y * v->y + p->z * v->z; }

static void API c_negative(V4 *o, const V4 *c)
{
    o->x = 1.0f - c->x; o->y = 1.0f - c->y; o->z = 1.0f - c->z; o->w = c->w;
}
static void API c_modulate(V4 *o, const V4 *a, const V4 *b)
{
    o->x = a->x * b->x; o->y = a->y * b->y; o->z = a->z * b->z; o->w = a->w * b->w;
}

static const struct {
    const char *name;
    void *fn;
} inlines[] = {
    {"VB_D3DXVec2Length", v2_length},       {"VB_D3DXVec2LengthSq", v2_length_sq},
    {"VB_D3DXVec2Dot", v2_dot},             {"VB_D3DXVec2CCW", v2_ccw},
    {"VB_D3DXVec2Add", v2_add},             {"VB_D3DXVec2Subtract", v2_sub},
    {"VB_D3DXVec2Minimize", v2_min},        {"VB_D3DXVec2Maximize", v2_max},
    {"VB_D3DXVec2Scale", v2_scale},         {"VB_D3DXVec2Lerp", v2_lerp},
    {"VB_D3DXVec3Length", v3_length},       {"VB_D3DXVec3LengthSq", v3_length_sq},
    {"VB_D3DXVec3Dot", v3_dot},             {"VB_D3DXVec3Cross", v3_cross},
    {"VB_D3DXVec3Add", v3_add},             {"VB_D3DXVec3Subtract", v3_sub},
    {"VB_D3DXVec3Minimize", v3_min},        {"VB_D3DXVec3Maximize", v3_max},
    {"VB_D3DXVec3Scale", v3_scale},         {"VB_D3DXVec3Lerp", v3_lerp},
    {"VB_D3DXVec4Length", v4_length},       {"VB_D3DXVec4LengthSq", v4_length_sq},
    {"VB_D3DXVec4Dot", v4_dot},             {"VB_D3DXVec4Add", v4_add},
    {"VB_D3DXVec4Subtract", v4_sub},        {"VB_D3DXVec4Minimize", v4_min},
    {"VB_D3DXVec4Maximize", v4_max},        {"VB_D3DXVec4Scale", v4_scale},
    {"VB_D3DXVec4Lerp", v4_lerp},           {"VB_D3DXMatrixIdentity", m_identity},
    {"VB_D3DXMatrixIsIdentity", m_is_identity},
    {"VB_D3DXQuaternionLength", v4_length}, {"VB_D3DXQuaternionLengthSq", v4_length_sq},
    {"VB_D3DXQuaternionDot", v4_dot},       {"VB_D3DXQuaternionIdentity", q_identity},
    {"VB_D3DXQuaternionIsIdentity", q_is_identity},
    {"VB_D3DXQuaternionConjugate", q_conjugate},
    {"VB_D3DXPlaneDot", v4_dot},            {"VB_D3DXPlaneDotCoord", p_dot_coord},
    {"VB_D3DXPlaneDotNormal", p_dot_normal},
    {"VB_D3DXColorNegative", c_negative},   {"VB_D3DXColorAdd", v4_add},
    {"VB_D3DXColorSubtract", v4_sub},       {"VB_D3DXColorScale", v4_scale},
    {"VB_D3DXColorModulate", c_modulate},   {"VB_D3DXColorLerp", v4_lerp},
};

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
// `\??\Z:\...` form; the real dx8vb is the i386 builtin in one of them.
static HMODULE load_wine_dx8vb(void)
{
    static const WCHAR suffix[] = u"\\i386-windows\\dx8vb.dll";
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

static HMODULE load_d3dx9(void)
{
    static WCHAR d3dx9[] = u"d3dx9_43.dll";
    UNICODE_STRING file = {sizeof(d3dx9) - 2, sizeof(d3dx9), d3dx9};
    HMODULE module = 0;
    LdrLoadDll(0, 0, &file, &module);
    return module;
}

int __stdcall DllMain(HMODULE self, ULONG reason, void *reserved)
{
    (void)reserved;
    if (reason != DLL_PROCESS_ATTACH)
        return 1;
    LdrDisableThreadCalloutsForDll(self);
    HMODULE wine = load_wine_dx8vb();
    HMODULE d3dx9 = load_d3dx9();
    for (unsigned i = 0; i < export_count; i++) {
        const char *name = names[i];
        void *address = 0;
        for (unsigned j = 0; j < sizeof(inlines) / sizeof(inlines[0]) && !address; j++)
            if (equal(name, inlines[j].name))
                address = inlines[j].fn;
        if (!address && d3dx9 && name[0] == 'V' && name[1] == 'B' && name[2] == '_')
            address = lookup(d3dx9, equal(name, "VB_D3DXMatrixfDeterminant") ? "D3DXMatrixDeterminant"
                                                                                : name + 3);
        if (!address && wine)
            address = lookup(wine, name);
        if (address)
            slots[i] = address;
    }
    return 1;
}
