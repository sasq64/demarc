// GLSL patching for opengl32.dll: a small backtracking regex engine and the
// patch list from DEMARC_GLSL_PATCHES. Freestanding, no libc.

#include "glsl.h"

#define MAX_GROUPS 10
#define INF 0xffff

enum { CHR, ANY, SET, BOL, EOL, WORDB, GRP, ALT };

typedef struct Node {
    unsigned char type, greedy;
    short group;
    unsigned short min, max;
    unsigned char set[32];
    struct Node *next, *sub;
} Node;

static unsigned length(const char *s)
{
    unsigned n = 0;
    while (s[n])
        n++;
    return n;
}

static Node *node(int type)
{
    Node *n = glsl_alloc(sizeof(Node));
    if (!n)
        return 0;
    for (unsigned i = 0; i < sizeof(Node); i++)
        ((char *)n)[i] = 0;
    n->type = type;
    n->greedy = 1;
    n->group = -1;
    n->min = n->max = 1;
    return n;
}

static void free_nodes(Node *n)
{
    while (n) {
        Node *next = n->next;
        free_nodes(n->sub);
        glsl_release(n);
        n = next;
    }
}

static void set_add(unsigned char *set, int lo, int hi)
{
    for (int c = lo; c <= hi; c++)
        set[c >> 3] |= 1 << (c & 7);
}

static int is_word(int c)
{
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_';
}

// \d \w \s and their negations into `set`; 0 if `c` is none of them.
static int class_escape(unsigned char *set, char c)
{
    unsigned char tmp[32] = {0};
    switch (c | 0x20) {
    case 'd':
        set_add(tmp, '0', '9');
        break;
    case 'w':
        set_add(tmp, '0', '9'), set_add(tmp, 'a', 'z'), set_add(tmp, 'A', 'Z'), set_add(tmp, '_', '_');
        break;
    case 's':
        set_add(tmp, '\t', '\r'), set_add(tmp, ' ', ' ');
        break;
    default:
        return 0;
    }
    for (int i = 0; i < 32; i++)
        set[i] |= c >= 'a' ? tmp[i] : (unsigned char)~tmp[i];
    return 1;
}

static char plain_escape(char c)
{
    return c == 'n' ? '\n' : c == 't' ? '\t' : c == 'r' ? '\r' : c;
}

typedef struct {
    const char *p;
    int groups, error;
} Parser;

static Node *parse_alts(Parser *ps, int group);

static int parse_number(Parser *ps)
{
    int n = -1;
    while (*ps->p >= '0' && *ps->p <= '9')
        n = (n < 0 ? 0 : n * 10) + (*ps->p++ - '0');
    return n;
}

static Node *parse_atom(Parser *ps)
{
    char c = *ps->p++;
    Node *n;
    if (c == '(') {
        int group = -1;
        if (ps->p[0] == '?' && ps->p[1] == ':')
            ps->p += 2;
        else if (++ps->groups < MAX_GROUPS)
            group = ps->groups;
        n = parse_alts(ps, group);
        if (!n || *ps->p != ')')
            ps->error = 1;
        else
            ps->p++;
        return n;
    }
    if (c == '.')
        return node(ANY);
    if (c == '^')
        return node(BOL);
    if (c == '$')
        return node(EOL);
    if (c == '[') {
        if (!(n = node(SET)))
            return 0;
        int negate = *ps->p == '^';
        ps->p += negate;
        for (int first = 1; *ps->p && (first || *ps->p != ']'); first = 0) {
            char lo = *ps->p++;
            if (lo == '\\' && *ps->p) {
                lo = *ps->p++;
                if (class_escape(n->set, lo))
                    continue;
                lo = plain_escape(lo);
            }
            char hi = lo;
            if (ps->p[0] == '-' && ps->p[1] && ps->p[1] != ']') {
                hi = ps->p[1];
                ps->p += 2;
                if (hi == '\\' && *ps->p)
                    hi = plain_escape(*ps->p++);
            }
            set_add(n->set, (unsigned char)lo, (unsigned char)hi);
        }
        if (*ps->p != ']')
            ps->error = 1;
        else
            ps->p++;
        if (negate)
            for (int i = 0; i < 32; i++)
                n->set[i] = ~n->set[i];
        return n;
    }
    if (c == '\\') {
        c = *ps->p++;
        if (!c) {
            ps->error = 1;
            return 0;
        }
        if (c == 'b')
            return node(WORDB);
        if ((n = node(SET)) && class_escape(n->set, c))
            return n;
        if (n)
            n->type = CHR, n->set[0] = plain_escape(c);
        return n;
    }
    if ((n = node(CHR)))
        n->set[0] = c;
    return n;
}

static void parse_quantifier(Parser *ps, Node *n)
{
    char c = *ps->p;
    if (c == '*')
        n->min = 0, n->max = INF;
    else if (c == '+')
        n->min = 1, n->max = INF;
    else if (c == '?')
        n->min = 0, n->max = 1;
    else if (c == '{') {
        const char *start = ps->p++;
        int lo = parse_number(ps), hi = lo;
        if (*ps->p == ',') {
            ps->p++;
            hi = parse_number(ps);
            if (hi < 0)
                hi = INF;
        }
        if (lo < 0 || *ps->p != '}' || hi < lo) {
            ps->p = start;  // a literal brace
            return;
        }
        n->min = lo, n->max = hi;
    } else
        return;
    ps->p++;
    if (n->type == BOL || n->type == EOL || n->type == WORDB)
        ps->error = 1;
    if (*ps->p == '?')
        n->greedy = 0, ps->p++;
}

// A group of `|`-separated alternatives, each a list of nodes under an ALT.
static Node *parse_alts(Parser *ps, int group)
{
    Node *g = node(GRP), **alt = g ? &g->sub : 0;
    if (!g)
        return 0;
    g->group = group;
    for (;;) {
        Node *a = node(ALT), **tail = a ? &a->sub : 0;
        if (!a) {
            ps->error = 1;
            return g;
        }
        *alt = a;
        alt = &a->next;
        while (*ps->p && *ps->p != '|' && *ps->p != ')' && !ps->error) {
            Node *n = parse_atom(ps);
            if (n) {
                *tail = n;
                tail = &n->next;
            }
            if (!n || ps->error) {
                ps->error = 1;
                return g;
            }
            parse_quantifier(ps, n);
        }
        if (*ps->p != '|' || ps->error)
            return g;
        ps->p++;
    }
}

static Node *compile(const char *pattern)
{
    Parser ps = {pattern, 0, 0};
    Node *g = parse_alts(&ps, 0);
    if (ps.error || *ps.p) {
        free_nodes(g);
        return 0;
    }
    return g;
}

// Matching. A Cont is what follows the end of one pass through group `g`.
typedef struct Cont {
    const Node *g;
    unsigned count;
    const char *start;
    const struct Cont *up;
} Cont;

typedef struct {
    const char *text;
    const char *caps[2 * MAX_GROUPS];
    const char *end;
} Matcher;

static int match(Matcher *m, const Node *n, const char *s, const Cont *k);

static int one(const Matcher *m, const Node *n, const char *s)
{
    (void)m;
    unsigned char c = *s;
    if (!c)
        return 0;
    if (n->type == CHR)
        return c == n->set[0];
    if (n->type == ANY)
        return c != '\n';
    return n->set[c >> 3] >> (c & 7) & 1;
}

static int pass(Matcher *m, const Node *g, const char *s, unsigned count, const Cont *up)
{
    for (const Node *a = g->sub; a; a = a->next) {
        Cont k = {g, count + 1, s, up};
        const char *saved = g->group >= 0 ? m->caps[2 * g->group] : 0;
        if (g->group >= 0)
            m->caps[2 * g->group] = s;
        if (match(m, a->sub, s, &k))
            return 1;
        if (g->group >= 0)
            m->caps[2 * g->group] = saved;
    }
    return 0;
}

// After `count` passes through `g`, at `s`: another pass or what follows.
static int repeat(Matcher *m, const Node *g, const char *s, unsigned count, const Cont *up,
                  int progressed)
{
    int more = count < g->max && progressed;
    if (count < g->min)
        return pass(m, g, s, count, up);
    if (g->greedy)
        return (more && pass(m, g, s, count, up)) || match(m, g->next, s, up);
    return match(m, g->next, s, up) || (more && pass(m, g, s, count, up));
}

static int match(Matcher *m, const Node *n, const char *s, const Cont *k)
{
    if (!n) {
        if (!k) {
            m->end = s;
            return 1;
        }
        const Node *g = k->g;
        const char *saved = g->group >= 0 ? m->caps[2 * g->group + 1] : 0;
        if (g->group >= 0)
            m->caps[2 * g->group + 1] = s;
        if (repeat(m, g, s, k->count, k->up, s != k->start))
            return 1;
        if (g->group >= 0)
            m->caps[2 * g->group + 1] = saved;
        return 0;
    }
    switch (n->type) {
    case GRP:
        return repeat(m, n, s, 0, k, 1);
    case BOL:
        return (s == m->text || s[-1] == '\n') && match(m, n->next, s, k);
    case EOL:
        return (!*s || *s == '\n') && match(m, n->next, s, k);
    case WORDB: {
        int before = s > m->text && is_word((unsigned char)s[-1]);
        return before != is_word((unsigned char)*s) && match(m, n->next, s, k);
    }
    }
    unsigned most = 0;
    while (most < n->max && one(m, n, s + most))
        most++;
    if (most < n->min)
        return 0;
    if (n->greedy) {
        for (unsigned i = most + 1; i-- > n->min;)
            if (match(m, n->next, s + i, k))
                return 1;
    } else {
        for (unsigned i = n->min; i <= most; i++)
            if (match(m, n->next, s + i, k))
                return 1;
    }
    return 0;
}

static int find(Matcher *m, const Node *re, const char *from)
{
    for (const char *s = from;; s++) {
        for (int i = 0; i < 2 * MAX_GROUPS; i++)
            m->caps[i] = 0;
        if (match(m, re, s, 0))
            return 1;
        if (!*s)
            return 0;
    }
}

// A growing output string.
typedef struct {
    char *data;
    unsigned len, cap;
} Buf;

static void put(Buf *b, const char *s, unsigned n)
{
    if (!b->data && b->cap)
        return;  // an allocation failed
    if (b->len + n + 1 > b->cap) {
        unsigned cap = (b->len + n + 1) * 2;
        char *data = glsl_alloc(cap);
        if (!data) {
            glsl_release(b->data);
            b->data = 0, b->cap = 1;
            return;
        }
        for (unsigned i = 0; i < b->len; i++)
            data[i] = b->data[i];
        glsl_release(b->data);
        b->data = data, b->cap = cap;
    }
    for (unsigned i = 0; i < n; i++)
        b->data[b->len++] = s[i];
    b->data[b->len] = 0;
}

int glsl_search(const char *text, const char *pattern)
{
    Node *re = compile(pattern);
    if (!re)
        return -1;
    Matcher m = {text, {0}, 0};
    int found = find(&m, re, text);
    free_nodes(re);
    return found;
}

char *glsl_replace(const char *text, const char *pattern, const char *replacement)
{
    Node *re = compile(pattern);
    if (!re)
        return 0;
    Matcher m = {text, {0}, 0};
    Buf out = {0};
    const char *s = text;
    while (find(&m, re, s)) {
        const char *at = m.caps[0], *end = m.end;
        put(&out, s, at - s);
        for (const char *r = replacement; *r; r++) {
            if (r[0] == '$' && r[1] >= '0' && r[1] <= '9') {
                int g = *++r - '0';
                const char *a = g ? m.caps[2 * g] : at, *b = g ? m.caps[2 * g + 1] : end;
                if (a && b)
                    put(&out, a, b - a);
            } else {
                r += r[0] == '$' && r[1] == '$';
                put(&out, r, 1);
            }
        }
        s = end;
        if (end == at) {
            if (!*s)
                break;
            put(&out, s++, 1);
        }
    }
    put(&out, s, length(s));
    free_nodes(re);
    return out.data;
}

// The patch list, split in place at its separators.
#define MAX_PATCHES 64
#define MAX_FIELDS 64

static struct {
    char *fields[MAX_FIELDS];
    unsigned count;
} patches[MAX_PATCHES];
static unsigned patch_count;

void glsl_configure(const char *config)
{
    unsigned n = length(config);
    char *copy = glsl_alloc(n + 1);
    if (!copy)
        return;
    for (unsigned i = 0; i <= n; i++)
        copy[i] = config[i];
    patch_count = 0;
    char *field = copy;
    for (char *p = copy;; p++) {
        if (*p && *p != GLSL_PATCH_SEP && *p != GLSL_FIELD_SEP)
            continue;
        char sep = *p;
        *p = 0;
        if (patch_count < MAX_PATCHES && patches[patch_count].count < MAX_FIELDS)
            patches[patch_count].fields[patches[patch_count].count++] = field;
        field = p + 1;
        if (sep != GLSL_FIELD_SEP && patch_count < MAX_PATCHES)
            patch_count++;
        if (!sep)
            break;
    }
}

static void hash_text(const char *s, char *hex)
{
    unsigned long long h = 0xcbf29ce484222325ull;
    while (*s)
        h = (h ^ (unsigned char)*s++) * 0x100000001b3ull;
    for (int i = 15; i >= 0; i--, h >>= 4)
        hex[i] = "0123456789abcdef"[h & 15];
    hex[16] = 0;
}

static void log_parts(const char *a, const char *b, const char *c)
{
    char line[256];
    unsigned n = 0;
    for (const char *part[] = {"opengl32: ", a, b, c}, **p = part; p < part + 4; p++)
        for (const char *s = *p; *s && n < sizeof(line) - 2; s++)
            line[n++] = *s;
    line[n++] = '\n';
    line[n] = 0;
    glsl_log(line);
}

char *glsl_patch(int count, const char *const *strings, const int *lengths)
{
    Buf src = {0};
    for (int i = 0; i < count; i++)
        if (strings[i])
            put(&src, strings[i], lengths && lengths[i] >= 0 ? (unsigned)lengths[i] : length(strings[i]));
    if (!src.data)
        return 0;
    char hash[17];
    hash_text(src.data, hash);
    log_parts("shader ", hash, "");

    char *text = src.data;
    int changed = 0;
    for (unsigned i = 0; i < patch_count; i++) {
        char **f = patches[i].fields;
        int hit = 0;
        if (f[0][0] == 'h') {
            hit = 1;
            for (int j = 0; j < 17; j++)
                hit &= hash[j] == f[0][1 + j];
        } else if (f[0][0] == 'm') {
            hit = glsl_search(src.data, f[0] + 1);
            if (hit < 0)
                log_parts("bad glsl pattern ", f[0] + 1, "");
        }
        if (hit != 1)
            continue;
        for (unsigned j = 1; j + 1 < patches[i].count; j += 2) {
            char *next = glsl_replace(text, f[j], f[j + 1]);
            if (!next) {
                log_parts("bad glsl pattern ", f[j], "");
                continue;
            }
            if (text != src.data)
                glsl_release(text);
            text = next;
        }
    }
    for (char *a = src.data, *b = text; !changed && (*a || *b); a++, b++)
        changed = *a != *b;
    if (text != src.data)
        glsl_release(src.data);
    if (!changed) {
        glsl_release(text);
        return 0;
    }
    log_parts("patched shader ", hash, "");
    return text;
}
