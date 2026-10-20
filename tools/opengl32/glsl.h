// GLSL source patching for shaders that today's compilers reject. See README.md.

#ifndef GLSL_H
#define GLSL_H

// Provided by whoever links glsl.c: opengl32.c inside wine, glsl_test.c natively.
void *glsl_alloc(unsigned size);
void glsl_release(void *p);
void glsl_log(const char *line);

// `config` is DEMARC_GLSL_PATCHES: patches separated by GLSL_PATCH_SEP, the
// fields of one by GLSL_FIELD_SEP. The first field is `h<hash>` or
// `m<regex>`, the rest are regex/replacement pairs.
#define GLSL_PATCH_SEP '\x1d'
#define GLSL_FIELD_SEP '\x1e'

void glsl_configure(const char *config);

// The shader with every matching patch applied, or 0 when none applies.
// Free with glsl_release.
char *glsl_patch(int count, const char *const *strings, const int *lengths);

// Every match of `pattern` in `text` replaced by `replacement`, where `$0`-`$9`
// are the match and its groups and `$$` is a dollar. 0 on a bad pattern.
char *glsl_replace(const char *text, const char *pattern, const char *replacement);

// Whether `pattern` matches anywhere in `text`; -1 on a bad pattern.
int glsl_search(const char *text, const char *pattern);

#endif
