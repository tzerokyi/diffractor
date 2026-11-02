#ifndef MZ_CONFIG_H
#define MZ_CONFIG_H

/* Diffractor platform configuration for minizip-ng (from mz_config.h.cmakein). */

/* Define to 1 if you have the <dirent.h> header file. */
#ifdef _WIN32
#define HAVE_DIRENT_H 0
#else
#define HAVE_DIRENT_H 1
#endif

/* Define to 1 if you have the <sys/dirent.h> header file. */
#define HAVE_SYS_DIRENT_H 0

/* Define to 1 if you have the <inttypes.h> header file. */
#define HAVE_INTTYPES_H 1

/* Define to 1 if you have the <stdint.h> header file. */
#define HAVE_STDINT_H 1

/* Define to 1 if DIR* is defined. */
#ifdef _WIN32
#define HAVE_PDIR 0
#define HAVE_FSEEKO 0
#define HAVE_SYMLINK 0
#define HAVE_READLINK 0
#else
#define HAVE_PDIR 1
#define HAVE_FSEEKO 1
#define HAVE_SYMLINK 1
#define HAVE_READLINK 1
#endif

#endif
