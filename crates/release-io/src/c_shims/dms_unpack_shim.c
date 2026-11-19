/*
 * dms_unpack_shim.c - turn a DMS archive into the ADF disk image inside it.
 *
 * The Rust side of this lives in src/newsys/dms.rs, and it exists to serve
 * `--unadf` (see `AmigaSystem::load`): both Amiga cores read `.dms` straight
 * off the disk, but ADFlib does not, so a release that ships as DMS has to be
 * turned back into a plain sector image before its file system can be walked.
 *
 * The unpacker itself is xDMS 1.3 by way of amiberry (external/dms); all this
 * adds is opening the two files and giving Rust a stable set of error codes.
 * The output is a normal `.adf`: 80 (or 160, for an HD archive) tracks of
 * 11264 bytes, written at their own offsets, so a DMS that is missing tracks
 * still yields an image with the tracks it does have in the right places.
 */

#include <stdio.h>

#include "cdata.h"
#include "pfile.h"

/* Error codes returned to Rust. Success is the size of the image written. */
#define DMS_ERR_OPEN  (-1) /* the archive could not be opened for reading */
#define DMS_ERR_WRITE (-2) /* the output could not be opened, or written */
#define DMS_ERR_NOTDMS (-3) /* not a DMS archive at all */
#define DMS_ERR_UNPACK (-4) /* it is one, but it did not come apart */
#define DMS_ERR_EMPTY (-5) /* it came apart into nothing */

/*
 * Unpack `dms_path` into a new file at `adf_path`, which is created (and
 * truncated if it exists). Returns the number of bytes written, or one of the
 * codes above.
 */
int demarc_dms_unpack(const char *dms_path, const char *adf_path)
{
    FILE *fi, *fo;
    USHORT ret;
    long size;

    fi = fopen(dms_path, "rb");
    if (fi == NULL)
        return DMS_ERR_OPEN;

    fo = fopen(adf_path, "w+b");
    if (fo == NULL) {
        fclose(fi);
        return DMS_ERR_WRITE;
    }

    ret = DMS_Process_File(fi, fo, CMD_UNPACK, OPT_QUIET, 0, 0, 0);

    size = -1;
    /* Flush before measuring: the last track is still in the stdio buffer. */
    if (fflush(fo) == 0 && fseek(fo, 0, SEEK_END) == 0)
        size = ftell(fo);

    fclose(fo);
    fclose(fi);

    if (ret == ERR_NOTDMS || ret == ERR_FMS)
        return DMS_ERR_NOTDMS;
    if (ret != NO_PROBLEM)
        return DMS_ERR_UNPACK;
    if (size < 0)
        return DMS_ERR_WRITE;
    if (size == 0)
        return DMS_ERR_EMPTY;
    return (int)size;
}
