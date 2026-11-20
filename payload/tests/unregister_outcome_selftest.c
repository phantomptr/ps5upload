/* Host self-test for unregister_outcome.h (#476): what an uninstall reports. */
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

#include "unregister_outcome.h"

static int reply(const unregister_outcome_t *o, char *body, size_t cap) {
    size_t w = 99;
    int r = unregister_reply(o, body, cap, &w);
    if (r >= 0) assert(w == strlen(body));
    else assert(w == 0);
    return r;
}

int main(void) {
    char body[96];
    char toast[160];

    /* A package uninstall Sony accepted and that left nothing behind. */
    unregister_outcome_t done = {.ours = 0, .api_called = 1, .sony_rc = 0, .still_present = 0};
    assert(unregister_verdict(&done) == UNREGISTER_DONE);
    assert(reply(&done, body, sizeof body) == 1);
    assert(strcmp(body, "{\"sony_uninstall_rc\":0,\"still_present\":false}") == 0);
    unregister_toast(&done, "CUSA33878", toast, sizeof toast);
    assert(strcmp(toast, "Uninstalled CUSA33878") == 0);

    /* Our folder registration keeps saying "Unregistered": its source files stay. */
    unregister_outcome_t ours = done;
    ours.ours = 1;
    unregister_toast(&ours, "PPSA01234", toast, sizeof toast);
    assert(strcmp(toast, "Unregistered PPSA01234") == 0);

    /* The reported case: Sony refused. Never an ACK, never "Unregistered". */
    unregister_outcome_t refused = {.ours = 0, .api_called = 1, .sony_rc = 0x80B21B02u,
                                    .still_present = 0};
    assert(unregister_verdict(&refused) == UNREGISTER_REFUSED);
    assert(reply(&refused, body, sizeof body) == 0);
    assert(strcmp(body, "uninstall_refused:0x80B21B02") == 0);
    unregister_toast(&refused, "CUSA33878", toast, sizeof toast);
    assert(strstr(toast, "Unregistered") == NULL && strstr(toast, "0x80B21B02") != NULL);

    /* Sony said 0 but the game is still there: an ACK that says so, no success toast. */
    unregister_outcome_t left = {.ours = 0, .api_called = 1, .sony_rc = 0, .still_present = 1};
    assert(unregister_verdict(&left) == UNREGISTER_STILL_PRESENT);
    assert(reply(&left, body, sizeof body) == 1);
    assert(strcmp(body, "{\"sony_uninstall_rc\":0,\"still_present\":true}") == 0);
    unregister_toast(&left, "CUSA33878", toast, sizeof toast);
    assert(strstr(toast, "Unregistered") == NULL && strstr(toast, "Uninstalled") == NULL);

    /* No uninstall API: fine when our teardown was all there was, an error otherwise. */
    unregister_outcome_t no_api_gone = {.ours = 1, .api_called = 0, .sony_rc = 0, .still_present = 0};
    assert(unregister_verdict(&no_api_gone) == UNREGISTER_DONE);
    unregister_outcome_t no_api_left = {.ours = 0, .api_called = 0, .sony_rc = 0, .still_present = 1};
    assert(unregister_verdict(&no_api_left) == UNREGISTER_UNAVAILABLE);
    assert(reply(&no_api_left, body, sizeof body) == 0);
    assert(strcmp(body, "uninstall_unavailable") == 0);

    /* A reply that does not fit is refused, not truncated. */
    assert(reply(&done, body, 8) == -1);

    /* Review fix: our folder registration whose teardown worked is done even when Sony
     * refused, as long as the title is no longer listed; the code still rides along. */
    unregister_outcome_t ours_refused_gone = {.ours = 1, .api_called = 1,
                                              .sony_rc = 0x80B21B02u, .still_present = 0};
    assert(unregister_verdict(&ours_refused_gone) == UNREGISTER_DONE);
    assert(reply(&ours_refused_gone, body, sizeof body) == 1);
    assert(strcmp(body, "{\"sony_uninstall_rc\":2159155970,\"still_present\":false}") == 0);
    unregister_toast(&ours_refused_gone, "PPSA01234", toast, sizeof toast);
    assert(strcmp(toast, "Unregistered PPSA01234") == 0);
    /* ... but not when the title is really still listed. */
    unregister_outcome_t ours_refused_left = ours_refused_gone;
    ours_refused_left.still_present = 1;
    assert(unregister_verdict(&ours_refused_left) == UNREGISTER_REFUSED);
    assert(reply(&ours_refused_left, body, sizeof body) == 0);
    /* A package refusal stays a refusal even if nothing is listed any more. */
    unregister_outcome_t pkg_refused_gone = ours_refused_gone;
    pkg_refused_gone.ours = 0;
    assert(unregister_verdict(&pkg_refused_gone) == UNREGISTER_REFUSED);

    /* A Sony home tile needs more than removal of our own registration metadata. */
    assert(unregister_tile_confirmed(&done));
    assert(!unregister_tile_confirmed(&ours_refused_gone));
    assert(!unregister_tile_confirmed(&left));
    assert(!unregister_tile_confirmed(&no_api_gone));

    /* Presence follows the engine's installed list. */
    unregister_presence_t p = {0};
    assert(!unregister_still_listed(&p));
    p.appmeta_dir = 1; /* an appmeta folder, with or without subfolders, is listed */
    assert(unregister_still_listed(&p));
    p = (unregister_presence_t){.app_dir = 1, .appdb_known = 1, .appdb_row = 1};
    assert(unregister_still_listed(&p)); /* a pkg install still in app.db */
    /* Our folder registration keeps /user/app/<id>/sce_sys by design: without an app.db row
     * the leftover folder is not a listed title (no false "still present"). */
    p = (unregister_presence_t){.app_dir = 1, .appdb_known = 1, .appdb_row = 0};
    assert(!unregister_still_listed(&p));
    /* app.db unreadable: the engine lists appmeta folders only, so does this. */
    p = (unregister_presence_t){.app_dir = 1, .appdb_known = 0};
    assert(!unregister_still_listed(&p));
    /* An app.db row without an app folder (a shell entry) is not listed either. */
    p = (unregister_presence_t){.appdb_known = 1, .appdb_row = 1};
    assert(!unregister_still_listed(&p));
    /* #879: what is left on the home screen counts. A visible app.db icon row with no folders
     * (the game's folders deleted by hand) is a tile the user still sees: never "gone". Before
     * #879 the helper judged by the PC app's list only and reported this as removed. */
    p = (unregister_presence_t){.appdb_known = 1, .appdb_row = 1, .appdb_icon_visible = 1};
    assert(!unregister_still_listed(&p));
    assert(unregister_still_present(&p));
    assert(!unregister_presence_unverified(&p));
    /* A hidden row (a shell entry) is no tile. */
    p = (unregister_presence_t){.appdb_known = 1, .appdb_row = 1, .appdb_icon_visible = 0};
    assert(!unregister_still_present(&p));
    /* Nothing listed and app.db unreadable: not checked, so not gone. */
    p = (unregister_presence_t){.app_dir = 1, .appdb_known = 0};
    assert(!unregister_still_present(&p));
    assert(unregister_presence_unverified(&p));
    p = (unregister_presence_t){.appmeta_dir = 1, .appdb_known = 0};
    assert(!unregister_presence_unverified(&p)); /* listed: known to be there */

    /* An unchecked result: an ACK that older engines read as "still there", and says why. */
    {
        unregister_outcome_t unchecked = {.api_called = 1, .sony_rc = 0, .still_present = 1,
                                          .unverified = 1};
        assert(unregister_verdict(&unchecked) == UNREGISTER_UNVERIFIED);
        assert(reply(&unchecked, body, sizeof body) == 1);
        assert(strcmp(body, "{\"sony_uninstall_rc\":0,\"still_present\":true,\"unverified\":true}") == 0);
        unregister_toast(&unchecked, "PPSA01234", toast, sizeof toast);
        assert(strstr(toast, "Uninstalled") == NULL && strstr(toast, "could not be checked") != NULL);
    }

    /* #879: who owns /user/app/<id>/mount.lnk. Before #879 every mount.lnk counted as ours, and
     * an uninstall of a ShadowMount+ title took its bind and links away from under it. */
    assert(unregister_tracker_owner(NULL, NULL) == UNREGISTER_TRACKER_NONE);
    /* ShadowMount+ image title: mount.lnk under its mount root, mount_img.lnk the image. */
    assert(unregister_tracker_owner("/mnt/shadowmnt/DOOM_1a2b3c4d", "/data/homebrew/DOOM.exfat") ==
           UNREGISTER_TRACKER_SMP);
    assert(unregister_tracker_owner("/mnt/shadowmnt/pfsc/DOOM_1a2b3c4d/PPSA01234-app", NULL) ==
           UNREGISTER_TRACKER_SMP);
    /* An image link that is not mount.lnk's own text is SMP's too. */
    assert(unregister_tracker_owner("/mnt/elsewhere/DOOM", "/data/homebrew/DOOM.exfat") ==
           UNREGISTER_TRACKER_SMP);
    /* Ours: an image we mounted (both links the same) and a folder (no image link). */
    assert(unregister_tracker_owner("/mnt/ps5upload/DOOM", "/mnt/ps5upload/DOOM") ==
           UNREGISTER_TRACKER_OURS);
    assert(unregister_tracker_owner("/data/homebrew/DOOM", NULL) == UNREGISTER_TRACKER_OURS);
    assert(unregister_tracker_owner("", NULL) == UNREGISTER_TRACKER_OURS); /* unreadable: as before */
    /* A path that only starts with the same letters is not SMP's root. */
    assert(unregister_tracker_owner("/mnt/shadowmnt2/DOOM", NULL) == UNREGISTER_TRACKER_OURS);

    /* #879: unregister takes off a bind at exactly the title's own path, nothing else. */
    assert(unregister_bind_unmountable("/system_ex/app/PPSA01234", "/system_ex/app/PPSA01234",
                                       "nullfs"));
    /* nothing mounted there: statfs names the parent filesystem */
    assert(!unregister_bind_unmountable("/system_ex/app/PPSA01234", "/system_ex", "exfatfs"));
    /* another title's mount, even one whose id starts the same */
    assert(!unregister_bind_unmountable("/system_ex/app/PPSA0123", "/system_ex/app/PPSA01234",
                                        "nullfs"));
    assert(!unregister_bind_unmountable("/system_ex/app/PPSA01234", "/system_ex/app/PPSA0123",
                                        "nullfs"));
    /* Sony's own mount of an installed copy */
    assert(!unregister_bind_unmountable("/system_ex/app/PPSA01234", "/system_ex/app/PPSA01234",
                                        "pfs"));

    /* A ShadowMount+ title: the reply and the toast say it comes back while its image is there. */
    {
        unregister_outcome_t smp = {.api_called = 1, .sony_rc = 0, .still_present = 0, .smp = 1};
        snprintf(smp.smp_image, sizeof smp.smp_image, "%s", "/data/homebrew/DOOM \"II\".exfat");
        char big[1400];
        assert(reply(&smp, big, sizeof big) == 1);
        assert(strcmp(big, "{\"sony_uninstall_rc\":0,\"still_present\":false,\"managed_by\":"
                           "\"shadowmount\",\"image\":\"/data/homebrew/DOOM \\\"II\\\".exfat\"}") == 0);
        unregister_toast(&smp, "PPSA01234", toast, sizeof toast);
        assert(strstr(toast, "ShadowMount+") != NULL && strstr(toast, "Uninstalled") == NULL);
        /* too small for the image: refused, never cut */
        assert(reply(&smp, body, sizeof body) == -1);
    }

    /* The roots are the engine's. */
    assert(sizeof UNREGISTER_APP_ROOTS / sizeof UNREGISTER_APP_ROOTS[0] == 3);
    assert(strcmp(UNREGISTER_APP_ROOTS[0], "/user/app") == 0);
    assert(strcmp(UNREGISTER_APP_ROOTS[1], "/mnt/ext0/user/app") == 0);
    assert(strcmp(UNREGISTER_APP_ROOTS[2], "/mnt/ext1/user/app") == 0);

    /* Review fix: an appmeta folder with a subfolder (changeinfo/) is removed whole, and a
     * symlink inside it is removed without touching what it points at. */
    {
        char root[] = "/tmp/ps5u-unreg-XXXXXX";
        assert(mkdtemp(root) != NULL);
        char meta[256], sub[256], f1[256], f2[256], outside[256], keep[256], link[256];
        snprintf(meta, sizeof meta, "%s/CUSA33878", root);
        snprintf(sub, sizeof sub, "%s/changeinfo", meta);
        snprintf(f1, sizeof f1, "%s/icon0.png", meta);
        snprintf(f2, sizeof f2, "%s/changeinfo.xml", sub);
        snprintf(outside, sizeof outside, "%s/outside", root);
        snprintf(keep, sizeof keep, "%s/keep.txt", outside);
        snprintf(link, sizeof link, "%s/link", meta);
        assert(mkdir(meta, 0755) == 0 && mkdir(sub, 0755) == 0 && mkdir(outside, 0755) == 0);
        FILE *fp;
        assert((fp = fopen(f1, "w")) != NULL); fclose(fp);
        assert((fp = fopen(f2, "w")) != NULL); fclose(fp);
        assert((fp = fopen(keep, "w")) != NULL); fclose(fp);
        assert(symlink(outside, link) == 0);
        unregister_remove_tree(meta, 3);
        struct stat st;
        assert(lstat(meta, &st) != 0);  /* gone, subfolder and link included */
        assert(stat(keep, &st) == 0);   /* the link's target is untouched */
        unlink(keep);
        rmdir(outside);
        rmdir(root);
    }

    puts("unregister outcome self-test passed");
    return 0;
}
