/*
 *     xDMS  v1.3  -  Portable DMS archive unpacker  -  Public Domain
 *     Written by     Andre Rodrigues de la Rocha  <adlroc@usa.net>
 *
 *     Handles the processing of a single DMS archive
 *
 *     This is amiberry's copy of the file (src/archivers/dms/pfile.cpp) with
 *     the host bindings swapped out, and nothing else touched. amiberry reads
 *     and writes through its own `struct zfile` and logs through `write_log`;
 *     neither exists here, so the I/O is plain stdio and the logging is gone.
 *     The other change is that the `extra` streams -- the banner, FILEID.DIZ
 *     and the fake boot block, which amiberry keeps to show the user -- are
 *     dropped: we only ever want the disk image. The decrunchers themselves
 *     (u_*.c, getbits.c, maketbl.c, tables.c, crc_csum.c) are verbatim.
 */


#define HEADLEN 56
#define THLEN 20
#define TRACK_BUFFER_LEN 32000
#define TEMP_BUFFER_LEN 32000


#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cdata.h"
#include "u_init.h"
#include "u_rle.h"
#include "u_quick.h"
#include "u_medium.h"
#include "u_deep.h"
#include "u_heavy.h"
#include "crc_csum.h"
#include "pfile.h"

#define DMSFLAG_ENCRYPTED 2
#define DMSFLAG_HD 16

static USHORT Process_Track(FILE *, FILE *, UCHAR *, UCHAR *, USHORT, int);
static USHORT Unpack_Track(UCHAR *, UCHAR *, USHORT, USHORT, UCHAR, UCHAR, USHORT, USHORT, USHORT, int);

static int passfound, passretries;

static USHORT PWDCRC;

UCHAR *dms_text;

/*  Length of what has been written to `fo` so far. The original asked the
 *  output zfile for its size; an image is written with seeks and can have
 *  holes in it, so track the high water mark rather than trust ftell.  */
static long outsize;

USHORT DMS_Process_File(FILE *fi, FILE *fo, USHORT cmd, USHORT opt, USHORT PCRC, USHORT pwd, int part)
{
	USHORT from, to, geninfo, cmode, hcrc, disktype, ret;
	ULONG unpkfsize;
	UCHAR *b1, *b2;

	/*  The two the original only used for its banner/FILEID.DIZ output and
	 *  its password prompt; kept in the signature so the call site still
	 *  reads like upstream's.  */
	(void)opt;
	(void)pwd;

	passfound = 0;
	passretries = 2;
	outsize = 0;
	b1 = (UCHAR *)calloc(TRACK_BUFFER_LEN, 1);
	if (!b1) return ERR_NOMEMORY;
	b2 = (UCHAR *)calloc(TRACK_BUFFER_LEN, 1);
	if (!b2) {
		free(b1);
		return ERR_NOMEMORY;
	}
	dms_text = (UCHAR *)calloc(TEMP_BUFFER_LEN, 1);
	if (!dms_text) {
		free(b1);
		free(b2);
		return ERR_NOMEMORY;
	}

	if (fread(b1,1,HEADLEN,fi) != HEADLEN) {
		free(b1);
		free(b2);
		free(dms_text);
		return ERR_SREAD;
	}

	if ( (b1[0] != 'D') || (b1[1] != 'M') || (b1[2] != 'S') || (b1[3] != '!') ) {
		/*  Check the first 4 bytes of file to see if it is "DMS!"  */
		free(b1);
		free(b2);
		free(dms_text);
		return ERR_NOTDMS;
	}

	hcrc = (USHORT)((b1[HEADLEN-2]<<8) | b1[HEADLEN-1]);
	/* Header CRC */

	if (hcrc != dms_CreateCRC(b1+4,(ULONG)(HEADLEN-6))) {
		free(b1);
		free(b2);
		free(dms_text);
		return ERR_HCRC;
	}

	geninfo = (USHORT) ((b1[10]<<8) | b1[11]);	/* General info about archive */
	from = (USHORT) ((b1[16]<<8) | b1[17]);		/*  Lowest track in archive. May be incorrect if archive is "appended" */
	to = (USHORT) ((b1[18]<<8) | b1[19]);		/*  Highest track in archive. May be incorrect if archive is "appended" */
	(void)to;

	if (part && from < 30) {
		free(b1);
		free(b2);
		free(dms_text);
		return DMS_FILE_END;
	}

	unpkfsize = (ULONG) ((((ULONG)b1[25])<<16) | (((ULONG)b1[26])<<8) | (ULONG)b1[27]);	/*  Length of unpacked data. Usually 901120 bytes  */

	disktype = (USHORT) ((b1[50]<<8) | b1[51]);		/*  Type of compressed disk  */
	cmode = (USHORT) ((b1[52]<<8) | b1[53]);        /*  Compression mode mostly used in this archive  */
	(void)cmode;                                    /*  per-track `cmode` is what actually decides  */

	PWDCRC = PCRC;

	if (disktype == 7) {
		/*  It's not a DMS compressed disk image, but a FMS archive  */
		free(b1);
		free(b2);
		free(dms_text);
		return ERR_FMS;
	}

	ret=NO_PROBLEM;

	Init_Decrunchers();

	if (cmd != CMD_VIEW) {
		Init_Decrunchers();
		for (;;) {
			ret = Process_Track(fi,fo,b1,b2,cmd,geninfo);
			if (ret == DMS_FILE_END)
				break;
			if (ret == NO_PROBLEM)
				continue;
			/* ignore posible extra data at the end of archive if output file is already complete */
			if ((ret == ERR_SREAD || ret == ERR_NOTTRACK || ret == ERR_THCRC || ret == ERR_BIGTRACK) && outsize >= (long)unpkfsize) {
				ret = DMS_FILE_END;
				break;
			}
			break;
		}
	}

	if (ret == DMS_FILE_END) ret = NO_PROBLEM;

	/*  Used to give an error message, but I have seen some DMS  */
	/*  files with texts or zeros at the end of the valid data   */
	/*  So, when we find something that is not a track header,   */
	/*  we suppose that the valid data is over. And say it's ok. */
	if (ret == ERR_NOTTRACK) ret = NO_PROBLEM;

	free(b1);
	free(b2);
	free(dms_text);

	return ret;
}

static USHORT Process_Track(FILE *fi, FILE *fo, UCHAR *b1, UCHAR *b2, USHORT cmd, int dmsflags){
	USHORT hcrc, dcrc, usum, number, pklen1, pklen2, unpklen, l;
	UCHAR cmode, flags;
	int crcerr = 0;
	int normaltrack;
	long pos;

	l = (USHORT)fread(b1,1,THLEN,fi);

	if (l != THLEN) {
		if (l==0)
			return DMS_FILE_END;
		else
			return ERR_SREAD;
	}

	/*  "TR" identifies a Track Header  */
	if ((b1[0] != 'T')||(b1[1] != 'R'))
		return ERR_NOTTRACK;

	/*  Track Header CRC  */
	hcrc = (USHORT)((b1[THLEN-2] << 8) | b1[THLEN-1]);

	if (dms_CreateCRC(b1,(ULONG)(THLEN-2)) != hcrc)
		return ERR_THCRC;

	number = (USHORT)((b1[2] << 8) | b1[3]);	/*  Number of track  */
	pklen1 = (USHORT)((b1[6] << 8) | b1[7]);	/*  Length of packed track data as in archive  */
	pklen2 = (USHORT)((b1[8] << 8) | b1[9]);	/*  Length of data after first unpacking  */
	unpklen = (USHORT)((b1[10] << 8) | b1[11]);	/*  Length of data after subsequent rle unpacking */
	flags = b1[12];		/*  control flags  */
	cmode = b1[13];		/*  compression mode used  */
	usum = (USHORT)((b1[14] << 8) | b1[15]);	/*  Track Data CheckSum AFTER unpacking  */
	dcrc = (USHORT)((b1[16] << 8) | b1[17]);	/*  Track Data CRC BEFORE unpacking  */

	if ((pklen1 > TRACK_BUFFER_LEN) || (pklen2 >TRACK_BUFFER_LEN) || (unpklen > TRACK_BUFFER_LEN))
		return ERR_BIGTRACK;

	if (fread(b1,1,(size_t)pklen1,fi) != pklen1)
		return ERR_SREAD;

	if (dms_CreateCRC(b1,(ULONG)pklen1) != dcrc)
		crcerr = 1;

	/*  track 80 is FILEID.DIZ, track 0xffff (-1) is Banner  */
	/*  and track 0 with 1024 bytes only is a fake boot block with more advertising */
	/*  FILE_ID.DIZ is never encrypted  */

	normaltrack = 0;
	if ((cmd == CMD_UNPACK) && (number<80) && (unpklen>2048)) {
		memset(b2, 0, unpklen);
		if (!crcerr) {
			Unpack_Track(b1, b2, pklen2, unpklen, cmode, flags, number, pklen1, usum, dmsflags & DMSFLAG_ENCRYPTED);
		}
		pos = (long)number * 512 * 22 * ((dmsflags & DMSFLAG_HD) ? 2 : 1);
		if (fseek(fo, pos, SEEK_SET) != 0)
			return ERR_CANTWRITE;
		if (fwrite(b2,1,(size_t)unpklen,fo) != unpklen)
			return ERR_CANTWRITE;
		if (pos + (long)unpklen > outsize)
			outsize = pos + (long)unpklen;
		normaltrack = 1;
	}

	if (!normaltrack)
		Init_Decrunchers();

	return NO_PROBLEM;

}



static USHORT Unpack_Track_2(UCHAR *b1, UCHAR *b2, USHORT pklen1, USHORT pklen2, USHORT unpklen, UCHAR cmode, UCHAR flags){
	switch (cmode){
		case 0:
			/*   No Compression   */
			memcpy(b2,b1,(size_t)unpklen);
			break;
		case 1:
			/*   Simple Compression   */
			if (Unpack_RLE(b1,b2, pklen1,unpklen)) return ERR_BADDECR;
			break;
		case 2:
			/*   Quick Compression   */
			if (Unpack_QUICK(b1,b2,pklen1,pklen2)) return ERR_BADDECR;
			if (Unpack_RLE(b2,b1,pklen2,unpklen)) return ERR_BADDECR;
			memcpy(b2,b1,(size_t)unpklen);
			break;
		case 3:
			/*   Medium Compression   */
			if (Unpack_MEDIUM(b1,b2,pklen1,pklen2)) return ERR_BADDECR;
			if (Unpack_RLE(b2,b1,pklen2,unpklen)) return ERR_BADDECR;
			memcpy(b2,b1,(size_t)unpklen);
			break;
		case 4:
			/*   Deep Compression   */
			if (Unpack_DEEP(b1,b2,pklen1,pklen2)) return ERR_BADDECR;
			if (Unpack_RLE(b2,b1,pklen2,unpklen)) return ERR_BADDECR;
			memcpy(b2,b1,(size_t)unpklen);
			break;
		case 5:
		case 6:
			/*   Heavy Compression   */
			if (cmode==5) {
				/*   Heavy 1   */
				if (Unpack_HEAVY(b1,b2,flags & 7,pklen1,pklen2)) return ERR_BADDECR;
			} else {
				/*   Heavy 2   */
				if (Unpack_HEAVY(b1,b2,flags | 8,pklen1,pklen2)) return ERR_BADDECR;
			}
			if (flags & 4) {
				memset(b1,0,unpklen);
				/*  Unpack with RLE only if this flag is set  */
				if (Unpack_RLE(b2,b1,pklen2,unpklen)) return ERR_BADDECR;
				memcpy(b2,b1,(size_t)unpklen);
			}
			break;
		default:
			return ERR_UNKNMODE;
	}

	if (!(flags & 1)) Init_Decrunchers();

	return NO_PROBLEM;

}

/*  DMS uses a lame encryption  */
static void dms_decrypt(UCHAR *p, USHORT len, UCHAR *src){
	USHORT t;

	while (len--){
		t = (USHORT) *src++;
		*p++ = t ^ (UCHAR)PWDCRC;
		PWDCRC = (USHORT)((PWDCRC >> 1) + t);
	}
}

static USHORT Unpack_Track(UCHAR *b1, UCHAR *b2, USHORT pklen2, USHORT unpklen, UCHAR cmode, UCHAR flags, USHORT number, USHORT pklen1, USHORT usum1, int enc)
{
	USHORT r, err = NO_PROBLEM;
	static USHORT pass;
	int maybeencrypted;
	int pwrounds;
	int firstpass = -1;
	UCHAR *tmp, *tmp_dms_text;
	USHORT prevpass = pass;

	if (passfound) {
		if (number != 80)
			dms_decrypt(b1, pklen1, b1);
		r = Unpack_Track_2(b1, b2, pklen1, pklen2, unpklen, cmode, flags);
		if (r == NO_PROBLEM) {
			if (usum1 == dms_Calc_CheckSum(b2,(ULONG)unpklen))
				return NO_PROBLEM;
		}
		if (passretries <= 0)
			return ERR_CSUM;
	}

	passretries--;
	pwrounds = 0;
	maybeencrypted = 0;
	tmp = (UCHAR *)malloc(pklen1 ? pklen1 : 1);
	tmp_dms_text = (UCHAR *)malloc(0x3fc8);
	if (!tmp || !tmp_dms_text) {
		free(tmp);
		free(tmp_dms_text);
		return ERR_NOMEMORY;
	}
	memcpy (tmp, b1, pklen1);
	memset(b2, 0, unpklen);
	memcpy(tmp_dms_text, dms_text, 0x3fc8);
	for (;;) {
		r = Unpack_Track_2(b1, b2, pklen1, pklen2, unpklen, cmode, flags);
		if (r == NO_PROBLEM) {
			if (usum1 == dms_Calc_CheckSum(b2,(ULONG)unpklen)) {
				passfound = maybeencrypted;
				/* if bootblock does not have "DOS", check other keys too */
				if (number > 0 || firstpass == pass || (b2[0] == 'D' && b2[1] == 'O' && b2[2] == 'S')) {
					err = NO_PROBLEM;
					pass = prevpass;
					break;
				}
				if (firstpass < 0) {
					firstpass = pass;
				}
			}
		}
		if (number == 80 || !enc) {
			err = ERR_CSUM;
			break;
		}
		maybeencrypted = 1;
		prevpass = pass;
		PWDCRC = pass;
		pass++;
		dms_decrypt(b1, pklen1, tmp);
		pwrounds++;
		if (pwrounds >= 65536) {
			if (firstpass < 0) {
				err = ERR_CSUM;
				passfound = 0;
				break;
			}
			pass = (USHORT)firstpass;
			PWDCRC = pass;
			dms_decrypt(b1, pklen1, tmp);
		}
		memcpy(dms_text, tmp_dms_text, 0x3fc8);
	}
	free(tmp_dms_text);
	free(tmp);
	return err;
}
