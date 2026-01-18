/* PS5-specific configuration for unrar
 * This file is included before any unrar headers
 */

#ifndef PS5_CONFIG_H
#define PS5_CONFIG_H

/* Force Unix mode */
#ifndef _UNIX
#define _UNIX
#endif

/* Disable console I/O - we're a library */
#ifndef SILENT
#define SILENT
#endif

/* Build as DLL/library */
#ifndef RARDLL
#define RARDLL
#endif

/* Disable SMP/threading for simplicity */
#undef RAR_SMP

/* PS5/FreeBSD is little endian ARM64 */
#ifndef LITTLE_ENDIAN
#define LITTLE_ENDIAN
#endif

/* ARM64 allows unaligned access */
#ifndef ALLOW_MISALIGNED
#define ALLOW_MISALIGNED
#endif

/* Disable features not needed for extraction */
#undef MOTW  /* Mark of the Web - Windows only */

/* FreeBSD compatibility */
#ifndef __FreeBSD__
#define __FreeBSD__ 1
#endif

#endif /* PS5_CONFIG_H */
