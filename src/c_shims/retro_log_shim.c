#include <stdarg.h>
#include <stdio.h>

extern void demarc_retro_log_rust(int level, const char *msg);

void demarc_retro_log_shim(int level, const char *fmt, ...) {
    char buf[4096];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    demarc_retro_log_rust(level, buf);
}
