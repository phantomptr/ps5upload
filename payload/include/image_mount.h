/* Whether a mount is an attached disk image, from what statfs says about it (fs.freespace's
 * is_image_mount policy). Pure, so the host self-test covers it. */
#ifndef PS5UPLOAD_IMAGE_MOUNT_H
#define PS5UPLOAD_IMAGE_MOUNT_H

#include <stddef.h>
#include <stdio.h>
#include <string.h>

/* A nullfs is followed through at most this many lower mounts. */
#define IMAGE_MOUNT_MAX_HOPS 4

enum {
    IMAGE_MOUNT_NO = 0,
    IMAGE_MOUNT_YES = 1,
    /* A nullfs view of another path: ask about the mount holding `from` instead. */
    IMAGE_MOUNT_FOLLOW = 2,
};

/* `fstype` and `from` are statfs's f_fstypename and f_mntfromname. An image is an LVD or MD device,
 * or a mount from an image mount root (ShadowMount+'s, our own). A nullfs from any other path (a
 * title's /system_ex/app link to an image mounted at /data/homebrew/<game>) is followed: taken as a
 * drive, it kept a drive's working margin, which on an image with 200 MiB spare left 0 bytes, and
 * every edit to the game was refused ("only 0 bytes are safely allocatable"). */
static inline int image_mount_kind(const char *fstype, const char *from) {
    if (!from || !from[0]) return IMAGE_MOUNT_NO;
    if (strncmp(from, "/dev/lvd", 8) == 0 || strncmp(from, "/dev/md", 7) == 0) return IMAGE_MOUNT_YES;
    if (strncmp(from, "/mnt/shadowmnt/", 15) == 0 || strncmp(from, "/mnt/ps5upload/", 15) == 0)
        return IMAGE_MOUNT_YES;
    if (fstype && strcmp(fstype, "nullfs") == 0 && from[0] == '/' && strncmp(from, "/dev/", 5) != 0)
        return IMAGE_MOUNT_FOLLOW;
    return IMAGE_MOUNT_NO;
}

/* Whether `from` (statfs's f_mntfromname) is one of the units fs.mount attaches an image to:
 * "/dev/lvd<N>" or "/dev/md<N>". fs.unmount takes a mount off in passes (an image can sit on another
 * image at one path), and after the first pass it may only go on while what is mounted there is still
 * one of these: the drive, the nullfs or the system folder that our mount was put on top of is not
 * ours to unmount, least of all with MNT_FORCE. */
static inline int image_mount_from_is_unit(const char *from) {
    if (!from) return 0;
    if (strncmp(from, "/dev/lvd", 8) == 0) return from[8] >= '0' && from[8] <= '9';
    if (strncmp(from, "/dev/md", 7) == 0) return from[7] >= '0' && from[7] <= '9';
    return 0;
}

/* Whether a mount whose source is `from` (statfs's f_mntfromname) lives on the device node
 * `device`: the node itself, or a slice / partition of it ("/dev/da1p1", "/dev/da1s1" on
 * "/dev/da1"). "/dev/da12" is another drive, not part of "/dev/da1". fs.fsck asks before a repair:
 * a repair writes to the device, and writing under a mounted file system corrupts it. */
static inline int image_mount_from_is_on_device(const char *device, const char *from) {
    size_t n;
    if (!device || !device[0] || !from) return 0;
    n = strlen(device);
    if (strncmp(from, device, n) != 0) return 0;
    return from[n] == '\0' || !(from[n] >= '0' && from[n] <= '9');
}

/* Whether a mount tracker file may be believed for what is mounted on `mount_point` from `from`.
 *
 * A tracker's file name is made from the mount point, in two forms that can meet: the bare leaf
 * for /mnt/ps5upload/<leaf>, and the path without its first slash (other bytes hex-escaped) for
 * any other mount point. So the tracker of an image mounted as /mnt/ps5upload/data ("data.src",
 * what an image called data.exfat gets) is also the tracker of /data, and "user" of /user. With
 * the file alone as proof, fs.volumes listed the console's own /data as a mounted image and
 * fs.unmount accepted it, going on to unmount(MNT_FORCE). Trackers also outlive a restart.
 *
 * Outside /mnt/ps5upload/ a tracker therefore counts only while an image unit is mounted there:
 * every mount fs.mount makes is one (/dev/lvd<N> or /dev/md<N>), a drive or system folder never. */
static inline int image_mount_tracker_trusted(const char *mount_point, const char *from) {
    if (!mount_point) return 0;
    if (strncmp(mount_point, "/mnt/ps5upload/", 15) == 0 && mount_point[15] != '\0') return 1;
    return image_mount_from_is_unit(from);
}

/* The longest tracker key: a tracker is the file "<key>.src", and a file name holds 255 bytes. */
#define IMAGE_MOUNT_KEY_MAX 251
/* A longer key keeps this much of itself, then "_x" and 32 hex digits of a hash of the mount point. */
#define IMAGE_MOUNT_KEY_HEAD (IMAGE_MOUNT_KEY_MAX - 34)

/* The tracker file name (no extension) of the mount at `mount_point`, into `out` (at least
 * IMAGE_MOUNT_KEY_MAX + 1 bytes, or the key is ""). Three forms:
 *
 *   - /mnt/ps5upload/<leaf>: the leaf as it is (fs.mount lets no slash or dot into it);
 *   - any other mount point: the path without its first slash, every byte that is not a letter, a
 *     digit, '-' or '.' written "_HH" (its hex), so "/mnt/ext1/games/foo" is
 *     "mnt_2fext1_2fgames_2ffoo" and no two mount points share a key;
 *   - when that is longer than IMAGE_MOUNT_KEY_MAX (each byte of a Japanese or Chinese folder name
 *     takes three): its first IMAGE_MOUNT_KEY_HEAD bytes, cut between two bytes of the path, then
 *     "_x" and a hash of the whole mount point. "_x" is no escape ('x' is no hex digit), so such a
 *     key is never another mount point's plain key.
 *
 * The third form is new. The key used to be cut at 255 bytes instead: its tracker's name was then
 * too long to create, so the image was mounted with no tracker, the Volumes list did not show it
 * and fs.unmount refused it as "not our mount" until the console restarted; and two mount points
 * that began with the same 85 such bytes shared one key. A key of up to IMAGE_MOUNT_KEY_MAX bytes is
 * the same as it was, so the trackers of mounts made by an earlier helper still resolve. */
static inline void image_mount_tracker_key(const char *mount_point, char *out, size_t out_cap) {
    static const char HEX[] = "0123456789abcdef";
    static const char BASE[] = "/mnt/ps5upload/";
    size_t i, j = 0, head = 0, total = 0;
    unsigned long long h1 = 1469598103934665603ull, h2 = 0x9e3779b97f4a7c15ull;
    if (!out || out_cap == 0) return;
    out[0] = '\0';
    if (!mount_point || out_cap < IMAGE_MOUNT_KEY_MAX + 1) return;
    if (strncmp(mount_point, BASE, sizeof(BASE) - 1) == 0 && mount_point[sizeof(BASE) - 1] != '\0' &&
        strchr(mount_point + sizeof(BASE) - 1, '/') == NULL &&
        strlen(mount_point + sizeof(BASE) - 1) <= IMAGE_MOUNT_KEY_MAX) {
        snprintf(out, out_cap, "%s", mount_point + sizeof(BASE) - 1);
        return;
    }
    for (i = (mount_point[0] == '/') ? 1u : 0u; mount_point[i] != '\0'; i++) {
        unsigned char c = (unsigned char)mount_point[i];
        int plain = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') ||
                    c == '-' || c == '.';
        size_t n = plain ? 1u : 3u;
        if (total + n <= IMAGE_MOUNT_KEY_MAX) {
            if (plain) {
                out[j++] = (char)c;
            } else {
                out[j++] = '_';
                out[j++] = HEX[(c >> 4) & 0xF];
                out[j++] = HEX[c & 0xF];
            }
        }
        total += n;
        if (total <= IMAGE_MOUNT_KEY_HEAD) head = total;
    }
    if (total <= IMAGE_MOUNT_KEY_MAX) {
        out[j] = '\0';
        return;
    }
    for (i = 0; mount_point[i] != '\0'; i++) {
        unsigned char c = (unsigned char)mount_point[i];
        h1 = (h1 ^ c) * 1099511628211ull;
        h2 = (h2 ^ c) * 0x100000001b3ull;
        h2 ^= h2 >> 29;
    }
    snprintf(out + head, out_cap - head, "_x%016llx%016llx", h1, h2);
}

/* A caller's mount point rewritten, in place, the way the kernel's mount table names it: one slash
 * between components and no "." component ("/data//games/./foo" becomes "/data/games/foo"). 0, or
 * -1 for a path that cannot be a mount point: not absolute, "/" itself, one that ends in a slash,
 * or one with a ".." component (never followed lexically: path_policy.c).
 *
 * fs.mount kept the string as it was sent, and everything after compared it with the kernel's
 * name: the "something is already mounted here" check, the tracker, the Volumes list, and at
 * unmount the search for the image unit to detach. With a doubled slash none of them matched: a
 * second request stacked another image on the first, the mount was not listed, and unmounting it
 * left its LVD/MD unit attached. */
static inline int image_mount_point_normalise(char *p) {
    size_t r = 0, w = 0;
    if (!p || p[0] != '/') return -1;
    while (p[r] != '\0') {
        size_t start, len;
        while (p[r] == '/') r++;
        if (p[r] == '\0') return -1; /* "/", or a trailing slash */
        start = r;
        while (p[r] != '\0' && p[r] != '/') r++;
        len = r - start;
        if (len == 1 && p[start] == '.') {
            if (p[r] == '\0') return -1; /* ends in "/." */
            continue;
        }
        if (len == 2 && p[start] == '.' && p[start + 1] == '.') return -1;
        p[w++] = '/';
        memmove(p + w, p + start, len);
        w += len;
    }
    if (w == 0) return -1;
    p[w] = '\0';
    return 0;
}

/* What statfs says about the mount holding `path`: its f_fstypename and f_mntfromname copied into
 * `fstype` and `from` (NUL-terminated). Non-zero when statfs failed. */
typedef int (*image_mount_look_fn)(const char *path, char *fstype, size_t fstype_n, char *from,
                                   size_t from_n, void *ctx);

/* Whether the mount holding `path` is an attached disk image, following nullfs views at most
 * IMAGE_MOUNT_MAX_HOPS mounts down. The first look is at `path` itself, whatever its length (a copy
 * into a mount-name-sized buffer cut a long request path short and asked about the wrong
 * directory); only a mount's source, which statfs bounds, is copied for the hops after it. */
static inline int image_mount_walk(const char *path, image_mount_look_fn look, void *ctx) {
    char fstype[32];
    char from[256];
    char at[256];
    const char *cur = path;
    if (!path || !look) return 0;
    for (int hop = 0; hop <= IMAGE_MOUNT_MAX_HOPS; hop++) {
        if (look(cur, fstype, sizeof fstype, from, sizeof from, ctx) != 0) return 0;
        int kind = image_mount_kind(fstype, from);
        if (kind != IMAGE_MOUNT_FOLLOW) return kind == IMAGE_MOUNT_YES;
        if (strcmp(cur, from) == 0) return 0;
        snprintf(at, sizeof at, "%s", from);
        cur = at;
    }
    return 0;
}

#endif
