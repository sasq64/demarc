// Native tests for glsl.c, run by build.py before the DLL is built.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "glsl.h"

void *glsl_alloc(unsigned size) { return malloc(size); }
void glsl_release(void *p) { free(p); }
void glsl_log(const char *line) { (void)line; }

static int failures;

static void replace(const char *text, const char *pattern, const char *with, const char *want)
{
    char *got = glsl_replace(text, pattern, with);
    if (!got != !want || (got && strcmp(got, want))) {
        printf("FAIL: \"%s\" =~ s/%s/%s/ gave \"%s\", want \"%s\"\n", text, pattern, with,
               got ? got : "(null)", want ? want : "(null)");
        failures++;
    }
    free(got);
}

static void search(const char *text, const char *pattern, int want)
{
    int got = glsl_search(text, pattern);
    if (got != want) {
        printf("FAIL: \"%s\" =~ /%s/ gave %d, want %d\n", text, pattern, got, want);
        failures++;
    }
}

int main(void)
{
    replace("abc", "b", "X", "aXc");
    replace("aaa", "a", "b", "bbb");
    replace("pow(c,e1*4.);", "pow\\(c,([^;]*)\\);", "pow(c,vec4($1));", "pow(c,vec4(e1*4.));");
    replace("x = 1-c;", "(\\W)1-", "$11.-", "x = 1.-c;");
    replace("x = 1-c;", "\\b1-", "1.-", "x = 1.-c;");
    replace("ab12cd", "\\d+", "#", "ab#cd");
    replace("ab12cd", "[a-c]", "", "12d");
    replace("ab12cd", "[^a-c]", "", "abc");
    replace("foo bar", "(foo|bar)", "<$1>", "<foo> <bar>");
    replace("aXbXc", "X.*?", "-", "a-b-c");
    replace("aXbXc", "X.*", "-", "a-");
    replace("abab", "(?:ab){2}", "Y", "Y");
    replace("abababc", "(ab)+c", "[$1]", "[ab]");
    replace("aaaa", "a{2,3}", "b", "ba");
    replace("abc", "x*", "-", "-a-b-c-");
    replace("line1\nline2", "^l", "L", "Line1\nLine2");
    replace("a\nb", "$", ";", "a;\nb;");
    replace("cost", "s", "$$", "co$t");
    replace("a.b", "\\.", ",", "a,b");
    replace("a{b", "{", "(", "a(b");
    replace("(()", "(", "", 0);
    replace("ab", "a)", "", 0);
    replace("ab", "\\", "", 0);
    replace("aaa", "(a*)*b", "", "aaa");
    search("vec4 c=pow(rc+bc,2.*e3);", "pow\\(rc\\+bc", 1);
    search("vec4 c;", "pow", 0);
    search("x", "[", -1);

    const char *a[] = {"void main(){\n", "gl_FragColor=pow(c,e1*4.);\n}"};
    int lengths[] = {-1, 15};
    glsl_configure("mColor=po$\x1e" "po$\x1e" "PO" "\x1d" "h0000000000000000\x1e" "main\x1e" "nope");
    char *out = glsl_patch(2, a, lengths);
    if (!out || strcmp(out, "void main(){\ngl_FragColor=PO")) {
        printf("FAIL: patch gave \"%s\"\n", out ? out : "(null)");
        failures++;
    }
    free(out);
    glsl_configure("mnothing\x1e" "a\x1e" "b");
    if ((out = glsl_patch(2, a, 0))) {
        printf("FAIL: unmatched patch applied: \"%s\"\n", out);
        failures++;
    }
    free(out);

    if (failures)
        return 1;
    printf("glsl tests passed\n");
    return 0;
}
