/*
 * adf_unpack_shim.c - unpack an ADF disk image into a host directory.
 *
 * The Rust side of this lives in src/newsys/adf.rs, and the whole thing exists
 * to serve `--unadf` (see `AmigaSystem::load`): a demo that ships as a bootable
 * AmigaDOS floppy runs a good deal faster if its files are handed to the
 * emulator as a hard drive instead of as a disk the core has to seek around.
 *
 * The walk is C rather than Rust because ADFlib's directory cursor is a field
 * of `struct AdfVolume` (`curDirPtr`, moved by adfChangeDir/adfParentDir) and
 * `adfFileOpen` resolves names against it. Driving that from Rust would mean
 * duplicating ADFlib's struct layouts in a bindings file that no compiler
 * checks against the headers; here they come from the headers themselves.
 * Modelled on `extract_tree()` in ADFlib's own examples/unadf.c.
 *
 * Everything the caller can't trust is refused rather than sanitised into
 * something plausible: see `safe_name`. The image is opened read-only and is
 * never written back.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "adflib.h"

#ifdef _WIN32
#include <direct.h>
#define MKDIR(path) _mkdir(path)
#else
#include <sys/stat.h>
#include <sys/types.h>
#define MKDIR(path) mkdir(path, 0777)
#endif

/* Error codes returned to Rust. Any success is a count of entries extracted. */
#define ADF_ERR_OPEN   (-1)  /* not an image ADFlib can open at all */
#define ADF_ERR_MOUNT  (-2)  /* no AmigaDOS volume on it (a trackloaded demo) */
#define ADF_ERR_IO     (-3)  /* the host side of the copy failed */

#define COPY_BUF_SIZE 8192

/* A directory tree deeper than this is a corrupt image, not a demo. */
#define MAX_DEPTH 16

/* An 880K floppy cannot hold more than 880K of file data, so anything past a
 * few megabytes means the block chains are looping and adfFileRead is never
 * going to reach EOF. Bounded so a bad image fails instead of filling the disk. */
#define MAX_TOTAL_BYTES (16u * 1024u * 1024u)

/*
 * Copy one Amiga file name into `out` as UTF-8, or refuse it.
 *
 * The names come from the image, so they are attacker-controlled as far as this
 * code is concerned, and they are about to be pasted onto a path. AmigaDOS
 * allows almost every byte in a file name, including the ones that would make
 * this an escape from the destination directory. Rather than rewrite those into
 * something that merely looks safe, refuse the entry: a demo disk holding a
 * file called `..` is not one we want to boot anyway.
 *
 * The bytes that are kept are transcoded from ISO 8859-1 to UTF-8, because that
 * is the boundary both Amiga cores draw: an Amiga file name is Latin-1, a host
 * file name is UTF-8, and each converts between the two on every call into the
 * file system. amiberry's `my_readdir` runs the host name through
 * `utf8_to_latin1_string` and *skips the entry* when that fails, and puae's
 * runs it through `utf8_to_local_string_alloc`, so a name written out as raw
 * Latin-1 -- `3d-demo.adf` ships one, `Har vi r\xf8get hash?` -- is not a file
 * the emulated Amiga can see at all. Writing the same bytes the cores would
 * write is what makes it visible.
 *
 * Returns 1 if `out` holds a usable name, 0 if the entry should be skipped.
 */
static int safe_name(const char *name, char *out, size_t cap)
{
    if (name == NULL)
        return 0;

    size_t len = strlen(name);
    if (len == 0)
        return 0;

    /* Would climb out of the destination directory. */
    if (strcmp(name, ".") == 0 || strcmp(name, "..") == 0)
        return 0;

    size_t o = 0;
    for (size_t i = 0; i < len; i++) {
        unsigned char c = (unsigned char) name[i];
        /* Path separators (both kinds -- the drive is also read on Windows),
         * the AmigaDOS volume separator, and controls including NUL padding. */
        if (c == '/' || c == '\\' || c == ':' || c < 0x20 || c == 0x7f)
            return 0;
        if (c < 0x80) {
            if (o + 1 >= cap)
                return 0;
            out[o++] = (char) c;
        } else {
            /* Latin-1 only ever reaches U+00FF, so two bytes always suffice. */
            if (o + 2 >= cap)
                return 0;
            out[o++] = (char) (0xc0 | (c >> 6));
            out[o++] = (char) (0x80 | (c & 0x3f));
        }
    }
    out[o] = '\0';
    return 1;
}

/* `dir/name`, into a freshly malloc'd string. NULL if out of memory. */
static char *join_path(const char *dir, const char *name)
{
    size_t dir_len = strlen(dir);
    size_t name_len = strlen(name);
    char *path = malloc(dir_len + 1 + name_len + 1);
    if (path == NULL)
        return NULL;
    memcpy(path, dir, dir_len);
    path[dir_len] = '/';
    memcpy(path + dir_len + 1, name, name_len);
    path[dir_len + 1 + name_len] = '\0';
    return path;
}

/*
 * Copy the file named `name` in the volume's current directory to `out_path`.
 * Returns 0, or ADF_ERR_IO if the host write failed. A file the image cannot
 * produce is skipped rather than failing the whole unpack: half a release is
 * still worth booting, and the caller checks for a startup-sequence anyway.
 */
static int extract_file(struct AdfVolume *vol,
                        const char *name,
                        const char *out_path,
                        unsigned *total)
{
    struct AdfFile *file = adfFileOpen(vol, name, ADF_FILE_MODE_READ);
    if (file == NULL)
        return 0;

    FILE *out = fopen(out_path, "wb");
    if (out == NULL) {
        adfFileClose(file);
        return ADF_ERR_IO;
    }

    int rc = 0;
    uint8_t buf[COPY_BUF_SIZE];
    while (!adfFileAtEOF(file)) {
        unsigned n = adfFileRead(file, sizeof(buf), buf);
        if (n == 0)
            break;
        if (*total > MAX_TOTAL_BYTES - n) {
            rc = ADF_ERR_IO;
            break;
        }
        *total += n;
        if (fwrite(buf, 1, n, out) != n) {
            rc = ADF_ERR_IO;
            break;
        }
    }

    if (fclose(out) != 0)
        rc = ADF_ERR_IO;
    adfFileClose(file);
    return rc;
}

/*
 * Extract the volume's current directory into `dest`, recursing into
 * subdirectories. Returns the number of entries written, or a negative error.
 */
static int unpack_dir(struct AdfVolume *vol,
                      const char *dest,
                      int depth,
                      unsigned *total)
{
    if (depth >= MAX_DEPTH)
        return 0;

    struct AdfList *list = adfGetDirEnt(vol, vol->curDirPtr);
    if (list == NULL)
        return 0;

    int count = 0;
    int rc = 0;
    for (struct AdfList *node = list; node != NULL; node = node->next) {
        struct AdfEntry *entry = node->content;
        if (entry == NULL)
            continue;

        /* Hard and soft links point outside the entry they sit in; following
         * them is how a walk ends up in a loop or outside `dest`. */
        if (entry->type != ADF_ST_FILE && entry->type != ADF_ST_DIR)
            continue;

        /* Twice the longest AmigaDOS name, since every byte may transcode
         * to two, plus room for the terminator. */
        char name[512];
        if (!safe_name(entry->name, name, sizeof(name)))
            continue;

        char *out_path = join_path(dest, name);
        if (out_path == NULL) {
            rc = ADF_ERR_IO;
            break;
        }

        if (entry->type == ADF_ST_DIR) {
            if (MKDIR(out_path) == 0 &&
                adfChangeDir(vol, entry->name) == ADF_RC_OK)
            {
                int sub = unpack_dir(vol, out_path, depth + 1, total);
                adfParentDir(vol);
                if (sub < 0)
                    rc = sub;
                else
                    count += sub + 1;
            }
        } else {
            rc = extract_file(vol, entry->name, out_path, total);
            if (rc == 0)
                count++;
        }

        free(out_path);
        if (rc != 0)
            break;
    }

    adfFreeDirList(list);
    return rc != 0 ? rc : count;
}

/*
 * Register ADFlib's built-in device drivers. Called once per process from Rust
 * (adfAddDeviceDriver appends to a global list, so calling it repeatedly would
 * grow that list without bound). Never paired with adfLibCleanUp.
 */
void demarc_adf_init(void)
{
    adfLibInit();
    /* ADFlib reports unreadable blocks and bad checksums straight to stderr,
     * and being handed a non-DOS demo disk is an expected outcome here, not
     * something to spew about: the caller falls back to booting the floppy. */
    adfEnvSetProperty(ADF_PR_QUIET, true);
}

/*
 * Unpack `adf_path` into the existing directory `dest_dir`.
 *
 * Returns the number of entries extracted (0 if the volume mounted but held no
 * files), or one of the negative ADF_ERR_* codes. Not re-entrant: ADFlib keeps
 * its environment in a global, so the Rust side holds a mutex across this.
 */
int demarc_adf_unpack(const char *adf_path, const char *dest_dir)
{
    struct AdfDevice *dev = adfDevOpen(adf_path, ADF_ACCESS_MODE_READONLY);
    if (dev == NULL)
        return ADF_ERR_OPEN;

    int result;
    if (adfDevMount(dev) != ADF_RC_OK) {
        result = ADF_ERR_MOUNT;
        goto close_dev;
    }

    /* Demo disks are single-volume floppies; anything else is out of scope. */
    if (dev->nVol < 1) {
        result = ADF_ERR_MOUNT;
        goto unmount_dev;
    }

    struct AdfVolume *vol = adfVolMount(dev, 0, ADF_ACCESS_MODE_READONLY);
    if (vol == NULL) {
        result = ADF_ERR_MOUNT;
        goto unmount_dev;
    }

    if (adfToRootDir(vol) != ADF_RC_OK) {
        result = ADF_ERR_MOUNT;
        goto unmount_vol;
    }

    unsigned total = 0;
    result = unpack_dir(vol, dest_dir, 0, &total);

unmount_vol:
    adfVolUnMount(vol);
unmount_dev:
    adfDevUnMount(dev);
close_dev:
    adfDevClose(dev);
    return result;
}
