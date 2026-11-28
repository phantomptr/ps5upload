// PS5Upload Play - every word the app shows, in one table (English now; another language is
// one more column). Plain words; "PS5Upload › …" names the screen to use for anything the app
// can't do itself.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

namespace play::ui
{
#define PLAY_STRINGS(X)                                                                                         \
    X(app_name, "PS5Upload Play")                                                                               \
    X(tagline, "Play together with friends, from the couch.")                                                   \
    X(home_join, "Join a friend's game")                                                                        \
    X(home_join_sub, "Use their code, or join a game on this network")                                         \
    X(home_host, "Host from this PS5")                                                                          \
    X(home_host_sub, "See who wants to join, share your invite, change the switches")                           \
    X(home_settings, "Settings")                                                                                \
    X(home_settings_sub, "Music, sounds and remembered computers")                                              \
    X(home_exit, "Close PS5Upload Play")                                                                        \
    X(hint_choose, "Choose")                                                                                    \
    X(hint_back, "Back")                                                                                        \
    X(hint_settings, "Settings")                                                                                \
    X(hint_forget, "Forget")                                                                                    \
    X(hint_refresh, "Look again")                                                                               \
    X(hint_type, "Type")                                                                                        \
    X(hint_delete, "Delete")                                                                                    \
    X(hint_join, "Join")                                                                                        \
    X(hint_change, "Change")                                                                                    \
    X(hint_allow, "Let in")                                                                                     \
    X(hint_deny, "Say no")                                                                                      \
    X(hint_remove, "Remove")                                                                                    \
    X(hint_retry, "Try again")                                                                                  \
    X(hint_ok, "OK")                                                                                            \
    X(pick_title_guest, "Which computer is your friend's game on?")                                             \
    X(pick_title_host, "Which computer runs PS5Upload for this PS5?")                                           \
    X(pick_looking, "Looking for PS5Upload on this network…")                                                   \
    X(pick_none, "No PS5Upload found yet. Make sure PS5Upload is open on the computer, and that \"PS5Upload Play can find this computer\" is on in PS5Upload › Settings › App › Play Together. You can also type its address.") \
    X(pick_found, "On this network")                                                                            \
    X(pick_used, "Used before")                                                                                 \
    X(pick_tailnet, "Over Tailscale")                                                                           \
    X(pick_tailnet_fail, "Couldn't reach %s over Tailscale (%s). Is Tailscale on in PS5Upload › Settings › App?")      \
    X(pick_type, "Type an address")                                                                             \
    X(pick_type_sub, "The address PS5Upload shows, like 192.168.1.20:19113")                                    \
    X(way_title, "Join a game on %s")                                                                           \
    X(way_code, "Enter a code")                                                                                 \
    X(way_code_sub, "Your friend reads it out from PS5Upload › Play › Play Together")                                  \
    X(way_nearby, "Ask to join without a code")                                                                 \
    X(way_nearby_sub, "Works when one Play Together is open on that computer")                                  \
    X(code_title, "Enter your friend's code")                                                                   \
    X(code_help, "Six letters and numbers, like KMX-4R7. Your friend sees it in PS5Upload › Play › Play Together.")    \
    X(code_join, "Join")                                                                                        \
    X(addr_title, "Type the computer's address")                                                                \
    X(addr_help, "PS5Upload shows it on its Play Together screen. Add :port if it is not 19113.")               \
    X(addr_bad, "That doesn't look like an address. Use numbers and dots, like 192.168.1.20.")                  \
    X(addr_ok, "Use this address")                                                                              \
    X(st_connecting, "Connecting to %s…")                                                                       \
    X(st_asking, "Waiting for your friend to let you in…")                                                      \
    X(st_asking_sub, "They see your request in PS5Upload and on their TV.")                                     \
    X(st_waiting, "Your friend is starting their game again…")                                                  \
    X(st_reconnecting, "Connection lost. Getting back in…")                                                     \
    X(way_lan, "Home network")                                                                                  \
    X(way_tailscale, "Tailscale")                                                                               \
    X(way_internet, "Internet")                                                                                 \
    X(way_bridge, "Internet, through your PS5Upload")                                                          \
    X(tier_excellent, "Excellent")                                                                              \
    X(tier_good, "Good")                                                                                        \
    X(tier_playable, "Playable")                                                                                \
    X(tier_poor, "Poor")                                                                                        \
    X(tier_down, "No answer")                                                                                   \
    X(way_chip, "%s · %s · %d ms")                                                                              \
    X(way_changed, "Now playing over %s")                                                                       \
    X(way_poor, "Slow connection: a cable or being nearer the router helps")                                   \
    X(st_picture, "Waiting for the picture…")                                                                   \
    X(st_joined, "You're Player %d on %s")                                                                      \
    X(st_your_friend, "your friend's PS5")                                                                      \
    X(why_key, "That code didn't work. Check it with your friend and try again.")                               \
    X(why_expired, "That invite has run out. Ask your friend for a new code.")                                  \
    X(why_full, "The game is full right now.")                                                                  \
    X(why_denied, "Your friend said no this time.")                                                             \
    X(why_no_answer, "Your friend didn't answer in time.")                                                      \
    X(why_closed, "Play Together isn't open on that computer right now.")                                       \
    X(why_not_playing, "Your friend isn't playing right now. They start Remote Play in PS5Upload first.")       \
    X(why_old_helper, "Your friend's PS5 needs a newer PS5Upload helper. They can update it in PS5Upload.")     \
    X(why_unreachable_ps5, "PS5Upload couldn't reach your friend's PS5.")                                       \
    X(why_slow_down, "Too many wrong codes. Wait a minute, then try again.")                                    \
    X(why_seat_taken, "Your friend needed that controller back. Join again to get your own.")                   \
    X(why_hello, "PS5Upload didn't understand this app. Update PS5Upload Play.")                                \
    X(why_other, "Your friend's computer said no (%s).")                                                        \
    X(fail_unreachable, "Couldn't reach PS5Upload at %s. Is it open, and on the same network as this PS5?")    \
    X(fail_busy, "PS5Upload is busy. Try again in a minute.")                                                   \
    X(fail_old_engine, "That PS5Upload is too old for PS5Upload Play. Update PS5Upload on that computer.")      \
    X(fail_closed, "PS5Upload closed the connection.")                                                          \
    X(end_left, "You left the game.")                                                                           \
    X(end_kicked, "Your friend took you out of the game.")                                                      \
    X(end_closed, "Your friend closed Play Together.")                                                          \
    X(end_replaced, "You joined from somewhere else, so this one stopped.")                                     \
    X(end_host_stopped, "Your friend stopped playing.")                                                         \
    X(end_lost, "The connection to your friend's game was lost.")                                               \
    X(end_seat_taken, "Your friend needed their controller back, and no other spot was free.")                  \
    X(play_hint_ps, "Hold OPTIONS + TOUCHPAD: your friend's PS button")                                         \
    X(play_hint_leave, "Hold L1 + R1 + OPTIONS + TOUCHPAD: leave")                                              \
    X(play_hold_ps, "Keep holding: your friend's PS button")                                                    \
    X(play_hold_leave, "Keep holding to leave")                                                                 \
    X(play_ps_sent, "PS button pressed on your friend's PS5")                                                   \
    X(play_move_first, "Move to your own user before pressing ✕ (not %s).")                                    \
    X(play_move_first_plain, "Move to your own user before pressing ✕.")                                       \
    X(play_plays_as, "Playing as %s")                                                                           \
    X(play_codec_hevc, "Your friend's stream uses a picture format this PS5 can't show. They can pick H.264 in PS5Upload › Play.") \
    X(play_codec_main10, "Your friend streams in HDR, which this app can't show yet. They can turn HDR off in PS5Upload › Play.") \
    X(play_took_host, "Your friend's stream ended: the user picked on the TV may have been theirs.")           \
    X(host_connecting, "Connecting to PS5Upload…")                                                              \
    X(host_pair_title, "Pair this PS5 with PS5Upload")                                                          \
    X(host_pair_text, "PS5Upload Play needs your OK once before it can show and change Play Together from here.") \
    X(host_pair_button, "Show a pairing code")                                                                  \
    X(host_pair_code_text, "On your computer, open Play › Play Together in PS5Upload, find the PS5Upload Play card, choose Pair and type this code.") \
    X(host_pair_left, "The code works for %d:%02d more.")                                                       \
    X(host_pair_gone, "The code ran out. Show a new one.")                                                      \
    X(host_auth_dropped, "PS5Upload no longer knows this PS5's pairing. Pair again.")                           \
    X(host_old_engine, "This PS5Upload doesn't know PS5Upload Play yet. Update PS5Upload on that computer.")    \
    X(host_retrying, "Lost PS5Upload. Trying again…")                                                           \
    X(host_session, "Remote Play")                                                                              \
    X(host_playing, "Streaming: %s, %d Mbit/s")                                                                 \
    X(host_idle, "Not streaming. Start Remote Play in PS5Upload › Play to play together.")                      \
    X(host_rp_ready, "Remote Play is set up on %s.")                                                            \
    X(host_rp_setup, "Set up Remote Play in PS5Upload › Play › Set up Play.")                                   \
    X(host_helper_on, "The PS5Upload helper is running on this PS5.")                                           \
    X(host_helper_off, "PS5Upload's helper isn't running here (or is older): start it from PS5Upload.") \
    X(host_rp_on_here, "Remote Play is on for this PS5.")                                                       \
    X(host_rp_off_here, "Remote Play is off on this PS5.")                                                      \
    X(host_rp_user_here, "This PS5 user hasn't allowed Remote Play yet.")                                       \
    X(host_rp_account, "This PS5's account isn't set up for Remote Play yet: use PS5Upload › Play › Set up Play.") \
    X(host_rp_enable, "Turn on Remote Play")                                                                    \
    X(host_rp_enable_confirm, "Press ✕ again to turn on Remote Play")                                           \
    X(host_rp_enable_failed, "Remote Play didn't turn on (%s). Use PS5Upload › Play › Set up Play.")            \
    X(host_rp_pin, "Show a Remote Play PIN")                                                                    \
    X(host_rp_pin_stop, "Stop the Remote Play PIN")                                                             \
    X(host_rp_pin_title, "Remote Play PIN")                                                                     \
    X(host_rp_pin_text, "Type it in PS5Upload › Play › Set up Play. %d:%02d left.")                             \
    X(host_rp_pin_paired, "Paired: PS5Upload can start Remote Play with this PS5 now.")                         \
    X(host_rp_pin_failed, "That PIN didn't work (%s). Show a new one.")                                         \
    X(host_together, "Play Together")                                                                           \
    X(host_together_closed, "Play Together is closed.")                                                         \
    X(host_together_open, "Open Play Together")                                                                 \
    X(host_together_wait, "Friends can join once you play. Start Remote Play in PS5Upload › Play.")             \
    X(host_code, "Invite code")                                                                                 \
    X(host_code_left, "Works for %d more minutes")                                                              \
    X(host_works_anywhere, "Scan to join from anywhere")                                                        \
    X(host_works_wifi, "Scan to join on this Wi-Fi")                                                            \
    X(host_new_code, "New invite code")                                                                         \
    X(host_close, "Close Play Together")                                                                        \
    X(host_asks, "%s wants to play")                                                                            \
    X(host_asks_ps5, "%s wants to play (on a PS5)")                                                             \
    X(host_friend, "A friend")                                                                                  \
    X(host_guest, "Player %d · %s")                                                                             \
    X(host_guest_rtt, "Player %d · %s · %d ms")                                                                 \
    X(host_no_guests, "No friends in yet.")                                                                     \
    X(host_sw_block, "Keep PS and Create to myself")                                                            \
    X(host_sw_max, "Friends at most")                                                                           \
    X(host_sw_seat, "A friend may use my stream's controller")                                                  \
    X(host_sw_invites, "Invites work")                                                                          \
    X(host_sw_awake, "Keep this PS5 awake")                                                                     \
    X(on, "On")                                                                                                 \
    X(off, "Off")                                                                                               \
    X(anywhere, "Anywhere")                                                                                     \
    X(wifi_only, "Only on my Wi-Fi")                                                                            \
    X(awake_off, "Off")                                                                                         \
    X(awake_transfers, "During transfers")                                                                      \
    X(awake_always, "Always")                                                                                   \
    X(host_wake_note, "To wake this PS5 from rest, use PS5Upload › Settings › This PS5.")                       \
    X(host_game_note, "While a game runs this app is closed by the PS5; join requests then show on the TV.")   \
    X(set_title, "Settings")                                                                                    \
    X(set_music, "Music in the menus")                                                                          \
    X(set_volume, "Music volume")                                                                               \
    X(set_sounds, "Button sounds")                                                                              \
    X(set_hints, "Show the controls while playing")                                                             \
    X(set_language, "Language")                                                                                 \
    X(lang_auto, "Same as this PS5")                                                                            \
    X(set_forget, "Forget remembered computers")                                                                \
    X(set_forgot, "Forgotten.")                                                                                 \
    X(set_about, "About")                                                                                       \
    X(about_version, "Version %s")                                                                              \
    X(about_music, "Music: an original piece made for PS5Upload Play.")                                         \
    X(about_credits, "Uses the JetBrains Mono and Noto Sans fonts (SIL OFL 1.1), Opus (BSD) and the QR Code generator library by Project Nayuki (MIT).") \
    X(about_license, "PS5Upload Play is free software under the GNU GPL v3.")

enum class S : int
{
#define PLAY_STR_ID(id, text) id,
    PLAY_STRINGS(PLAY_STR_ID)
#undef PLAY_STR_ID
        count_
};

const char *tr(S id) noexcept;
} // namespace play::ui
