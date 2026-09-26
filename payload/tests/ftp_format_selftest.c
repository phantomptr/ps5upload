/* Host-side test for the payload FTP server's wire formatting.
 *
 * The exact byte shapes matter more than they look: clients parse the
 * PASV and EPSV replies with strict regexes, and a listing line that
 * drifts from the ls(1) layout makes GUI clients show an empty folder.
 * Expected strings below were taken from live sessions against the
 * console and from RFC 959 / RFC 2428. */
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#include "../include/ftp_format.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

int main(void) {
    char buf[256];

    /* EPSV argument handling (RFC 2428 §3). Clients send a bare EPSV,
     * "EPSV 1" for IPv4, "EPSV 2" for IPv6, or "EPSV ALL". */
    CHECK(ftp_parse_epsv_arg(NULL) == FTP_EPSV_IPV4);
    CHECK(ftp_parse_epsv_arg("") == FTP_EPSV_IPV4);
    CHECK(ftp_parse_epsv_arg("1") == FTP_EPSV_IPV4);
    CHECK(ftp_parse_epsv_arg("ALL") == FTP_EPSV_ALL);
    CHECK(ftp_parse_epsv_arg("all") == FTP_EPSV_ALL);
    /* The PS5 socket is IPv4; asking for IPv6 must be refused, not
     * answered with an IPv4 port the client cannot use. */
    CHECK(ftp_parse_epsv_arg("2") == FTP_EPSV_BAD_PROTO);
    CHECK(ftp_parse_epsv_arg("banana") == FTP_EPSV_BAD_PROTO);

    /* PASV: the port is split into two bytes, high then low. This exact
     * string was observed from the console: port 62647 = 244*256 + 183. */
    {
        const unsigned char ip[4] = {192, 168, 86, 100};
        CHECK(ftp_format_pasv(ip, 62647, buf, sizeof(buf)) > 0);
        CHECK(strcmp(buf, "Entering Passive Mode (192,168,86,100,244,183).") == 0);
    }

    /* A low port must still split correctly. */
    {
        const unsigned char ip[4] = {10, 0, 0, 1};
        CHECK(ftp_format_pasv(ip, 258, buf, sizeof(buf)) > 0);
        CHECK(strcmp(buf, "Entering Passive Mode (10,0,0,1,1,2).") == 0);
    }

    /* EPSV: RFC 2428 uses three empty delimiter fields then the port. */
    CHECK(ftp_format_epsv(62647, buf, sizeof(buf)) > 0);
    CHECK(strcmp(buf, "Entering Extended Passive Mode (|||62647|)") == 0);

    /* Mode strings. */
    {
        char mode[11];
        ftp_format_mode(1, 0755, mode);
        CHECK(strcmp(mode, "drwxr-xr-x") == 0);
        ftp_format_mode(0, 0644, mode);
        CHECK(strcmp(mode, "-rw-r--r--") == 0);
        ftp_format_mode(0, 0777, mode);
        CHECK(strcmp(mode, "-rwxrwxrwx") == 0);
        ftp_format_mode(1, 0000, mode);
        CHECK(strcmp(mode, "d---------") == 0);
    }

    /* A listing line, matching what the console already emits. */
    CHECK(ftp_format_list_line(1, 0777, 65536, "Aug 09 14:05", "data", buf,
                               sizeof(buf)) > 0);
    CHECK(strcmp(buf, "drwxrwxrwx 1 root root 65536 Aug 09 14:05 data") == 0);

    CHECK(ftp_format_list_line(0, 0777, 132048, "Jul 17 19:58",
                               "decid_update.elf", buf, sizeof(buf)) > 0);
    CHECK(strcmp(buf, "-rwxrwxrwx 1 root root 132048 Jul 17 19:58 "
                      "decid_update.elf") == 0);

    /* A name too long for the buffer must be refused, not truncated into
     * a line that a client would parse as a different file. */
    {
        char small[32];
        CHECK(ftp_format_list_line(0, 0644, 1, "Aug 09 14:05",
                                   "a-very-long-file-name-that-will-not-fit",
                                   small, sizeof(small)) < 0);
    }

    printf("ftp_format_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
