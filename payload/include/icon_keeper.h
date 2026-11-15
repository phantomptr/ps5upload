#ifndef PS5UPLOAD_ICON_KEEPER_H
#define PS5UPLOAD_ICON_KEEPER_H

/* Home-screen icons the user hid through ps5upload stay hidden (#450).
 *
 * The system sets an app's icon visible again whenever the app starts, from ps5upload, the
 * home screen or a forwarder alike (measured on a FW 13.60 Phat: a hidden game's icon was
 * visible again within 5 s of its launch). The helper remembers which titles were hidden here,
 * in /data/ps5upload/hidden_icons.txt, and a thread hides them again whenever app.db changes. */

/* Remember (`hidden` 1) or forget (0) that the user hid `title_id`. */
void icon_keeper_note(const char *title_id, int hidden);

/* Show every icon on the list again and forget them, all but `except` (NULL: none), which
 * stays listed. Returns how many were shown ("Remove PS5Upload from this PS5"). */
int icon_keeper_show_all(const char *except);

/* Start the thread; it runs for the helper's lifetime. */
void icon_keeper_start(void);

#endif
