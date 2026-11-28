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
    X(home_join_sub, "Rooms, games on this network, invites and codes")                                         \
    X(home_host, "Host from this PS5")                                                                          \
    X(home_host_sub, "Your room: who's asking, who's playing, your invite")                           \
    X(home_settings, "Settings")                                                                                \
    X(home_settings_sub, "Music, sounds and remembered computers")                                              \
    X(home_exit, "Close PS5Upload Play")                                                                        \
    X(home_nearby, "Join %s's game")                                                                            \
    X(home_nearby_sub, "Playing now on this network. Your friend lets you in.")                                 \
    X(home_again, "Play again with %s")                                                                         \
    X(home_again_sub, "Your friend gets asked to let you in")                                                   \
    X(home_invite, "%s invites you to play")                                                                    \
    X(home_invite_sub, "Join now. Your friend lets you in.")                                                    \
    X(phone_title, "Invite on your phone?")                                                                     \
    X(phone_text, "Scan this with your phone, then tap Play on my PS5.")                                        \
    X(phone_wait, "Getting the phone code…")                                                                    \
    X(phone_offline, "The phone code needs the internet, and this PS5 can't reach it right now.")               \
    X(phone_again, "The phone code is off. Press △ to show it again.")                                          \
    X(hint_phone, "Phone code")                                                                                 \
    X(inv_got, "%s sent you an invite.")                                                                        \
    X(inv_unreachable, "This PS5 can't reach %s's game. It needs to be on this network or on Tailscale.")       \
    X(inv_bridge_need, "To play %s's game over the internet, this PS5 needs PS5Upload 13.0.0 or later open on a computer on your home network, with \"PS5Upload Play can find this computer\" on in PS5Upload › Settings › App › Play Room.") \
    X(why_friend, "Your friend's PS5Upload doesn't remember this PS5 any more. Join once with a code, then Play again works.") \
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
    X(pick_none, "No PS5Upload found yet. Make sure PS5Upload is open on the computer, and that \"PS5Upload Play can find this computer\" is on in PS5Upload › Settings › App › Play Room. You can also type its address.") \
    X(pick_found, "On this network")                                                                            \
    X(pick_used, "Used before")                                                                                 \
    X(pick_tailnet, "Over Tailscale")                                                                           \
    X(pick_tailnet_fail, "Couldn't reach %s over Tailscale (%s). Is Tailscale on in PS5Upload › Settings › App?")      \
    X(pick_type, "Type an address")                                                                             \
    X(pick_type_sub, "The address PS5Upload shows, like 192.168.1.20:19113")                                    \
    X(way_title, "Join a game on %s")                                                                           \
    X(way_code, "Enter a code")                                                                                 \
    X(way_code_sub, "Your friend reads it out from PS5Upload › Play › Play Room")                                  \
    X(way_nearby, "Ask to join without a code")                                                                 \
    X(way_nearby_sub, "Works when a Play Room is open on that computer")                                  \
    X(code_title, "Enter your friend's code")                                                                   \
    X(code_help, "Six letters and numbers, like KMX-4R7. Your friend sees it in PS5Upload › Play › Play Room.")    \
    X(code_join, "Join")                                                                                        \
    X(addr_title, "Type the computer's address")                                                                \
    X(addr_help, "PS5Upload shows it on its Play Room screen. Add :port if it is not 19113.")               \
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
    X(why_closed, "Your friend isn't hosting a Play Room right now.")                                       \
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
    X(fail_bridge_old, "PS5Upload at %s is too old to play over the internet. Update it to 13.0.0 or later.")   \
    X(fail_bridge_reach, "Couldn't reach PS5Upload at %s to play over the internet. Is it open on that computer?") \
    X(fail_bridge_hub, "Your PS5Upload couldn't reach the invite service. Check that computer's internet connection.") \
    X(fail_bridge_room, "Your friend's internet invite isn't open any more. Ask them for a new one.")           \
    X(fail_bridge_direct, "Your network and your friend's couldn't connect over the internet. Try again, or play over Tailscale.") \
    X(fail_bridge_other, "Your PS5Upload couldn't open the internet invite (%s).")                              \
    X(end_left, "You left the game.")                                                                           \
    X(end_kicked, "Your friend took you out of the game.")                                                      \
    X(end_closed, "Your friend closed the room.")                                                          \
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
    X(host_pair_text, "PS5Upload Play needs your OK once before it can show and change your Play Room from here.") \
    X(host_pair_button, "Show a pairing code")                                                                  \
    X(host_pair_code_text, "On your computer, PS5Upload shows this code in Play › Play Room: choose Yes, pair there, or type the code.") \
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
    X(host_together, "Play Room")                                                                           \
    X(host_together_closed, "Your Play Room is closed.")                                                         \
    X(host_together_open, "Open a Play Room")                                                                 \
    X(host_together_wait, "Friends can join once you play. Start Remote Play in PS5Upload › Play.")             \
    X(host_code, "Invite code")                                                                                 \
    X(host_code_left, "Works for %d more minutes")                                                              \
    X(host_works_anywhere, "Scan to join from anywhere")                                                        \
    X(host_works_wifi, "Scan to join on this Wi-Fi")                                                            \
    X(host_new_code, "New invite code")                                                                         \
    X(host_close, "Close the Play Room")                                                                        \
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
    X(about_credits, "Uses the JetBrains Mono and Noto Sans fonts (SIL OFL 1.1), Opus (BSD), BearSSL (MIT) and the QR Code generator library by Project Nayuki (MIT).") \
    X(about_license, "PS5Upload Play is free software under the GNU GPL v3.") \
    X(flag_sent, "%s: the host was told") \
    X(hint_flag, "Tell the host") \
    X(chat_too_long, "That line is too long (140 characters at most).") \
    X(voice_ptt_only, "Push-to-talk only in this room: hold Create.") \
    X(home_host_auto, "Host from this PS5 with PS5Upload on %s") \
    X(pair_grant_failed, "Pairing through the helper didn't work. Show a code instead.") \
    X(home_thanks, "Thanks & Support") \
    X(home_thanks_sub, "The people who keep PS5Upload going") \
    X(home_room_open, "%s's room is open") \
    X(home_room_open_sub, "Join now. Your friend lets you in.") \
    X(home_resume, "Back to %s's game") \
    X(home_resume_sub, "Your place is kept for a little while") \
    X(home_host_here, "PS5Upload on %s: host from here") \
    X(home_host_here_sub, "See who wants to join, share your invite, run your room") \
    X(home_pair, "Pair with PS5Upload on %s") \
    X(home_pair_sub, "Once, so this PS5 can run your room from the couch") \
    X(home_welcome, "How PS5Upload Play works") \
    X(home_welcome_sub, "Three short pages") \
    X(foot_engine, "PS5Upload on %s") \
    X(foot_looking, "Looking for PS5Upload…") \
    X(foot_helper, "Helper %s") \
    X(foot_no_helper, "No helper") \
    X(rooms_title, "Join") \
    X(rooms_sort, "Sort: %s") \
    X(sort_best, "Best for you") \
    X(sort_latency, "Lowest latency") \
    X(sort_game, "Game") \
    X(sort_players, "Most players") \
    X(sort_space, "Has space") \
    X(sort_newest, "Newest") \
    X(grp_friends, "Your friends' rooms") \
    X(grp_shared, "Shared with you") \
    X(grp_nearby, "Nearby") \
    X(grp_public, "Public rooms") \
    X(card_ask, "Ask to join") \
    X(card_join, "Join") \
    X(card_full, "Full") \
    X(card_update, "Update to join") \
    X(card_no_ps5, "Not open to PS5 guests") \
    X(card_about_ms, "about %d ms") \
    X(card_measured, "%d ms measured") \
    X(card_last, "last time %d ms") \
    X(card_pw, "Needs a password") \
    X(card_from, "%s invited you") \
    X(card_locked, "Locked") \
    X(rooms_code, "Enter a code") \
    X(rooms_address, "Type an address") \
    X(rooms_none, "No public rooms right now.") \
    X(rooms_none_sub, "Got an invite? Enter its code at the bottom of this list.") \
    X(rooms_nomatch, "No rooms match. Press △, then □ to clear the filters.") \
    X(rooms_unreachable, "Can't reach the rooms list. Check your internet. ✕ on this line tries again.") \
    X(rooms_off, "Your rooms service has no public rooms.") \
    X(rooms_loading, "Getting the rooms…") \
    X(rooms_more, "More rooms") \
    X(friends_none, "None of the people you've played with has a room open.") \
    X(rooms_intro, "Rooms are open games you can ask to join. The host decides who plays.") \
    X(rooms_need_home, "To join rooms over the internet, keep PS5Upload open on a computer at home.") \
    X(rooms_is_full, "The room is full.") \
    X(rooms_update, "Needs PS5Upload %s or newer. Update PS5Upload Play in PS5Upload or ProsperoStore.") \
    X(rooms_no_ps5, "This host doesn't take guests on a PS5 yet.") \
    X(app_newer, "A newer PS5Upload Play is available (%s).") \
    X(hint_details, "Details") \
    X(hint_filter, "Filter") \
    X(hint_sort, "Sort") \
    X(hint_clear, "Clear") \
    X(hint_close, "Close") \
    X(hint_menu, "Player menu") \
    X(hint_volume, "Volume") \
    X(hint_mute, "Mute") \
    X(hint_play_again, "Play again") \
    X(hint_home, "Home") \
    X(hint_next, "Next") \
    X(hint_skip, "Skip") \
    X(hint_help, "Help") \
    X(hint_allow_all, "Let all in") \
    X(hint_tabs, "Tabs") \
    X(det_title, "Details") \
    X(det_host, "Hosted by %s · %s") \
    X(det_playing, "Playing for %d min") \
    X(det_stream, "%s · %d fps · %s · %d Mbps") \
    X(det_you_measured, "Your connection to %s: %s · %d ms (measured)") \
    X(det_you_about, "Your connection to %s: about %d ms (estimate)") \
    X(det_you_unknown, "Your connection to %s isn't known yet.") \
    X(det_old, "More details need the host to update PS5Upload.") \
    X(det_no_direct, "Details need a direct connection right now.") \
    X(det_no_answer, "The host didn't answer.") \
    X(det_gone, "This room is closed.") \
    X(det_loading, "Getting the details…") \
    X(det_players, "Players") \
    X(det_seat, "Player %d · %s") \
    X(det_asks, "Asks the host") \
    X(det_pw, "Needs a password") \
    X(flt_title, "Filter") \
    X(flt_space, "Has space") \
    X(flt_nopw, "No password") \
    X(flt_myver, "Works with my version") \
    X(flt_near, "Near me") \
    X(flt_lang, "Language") \
    X(flt_any, "Any") \
    X(flt_game, "Game") \
    X(flt_game_any, "Any game") \
    X(flt_clear, "Clear filters") \
    X(kb_title_pw, "Password for %s") \
    X(kb_title_name, "Your name") \
    X(kb_title_chat, "Say something") \
    X(kb_title_game, "Look for a game") \
    X(kb_title_room, "Your room's name") \
    X(kb_title_newpw, "A password for your room") \
    X(kb_help, "✕ type · □ delete · △ space · L1 symbols · R1 capitals · R2 done") \
    X(kb_shift, "Shift") \
    X(kb_symbols, "?123") \
    X(kb_letters, "abc") \
    X(kb_space, "Space") \
    X(kb_done, "Done") \
    X(why_password, "Wrong password. %d tries left.") \
    X(why_password_plain, "Wrong password.") \
    X(why_locked_out, "Too many tries. Try again in 10 minutes.") \
    X(why_locked, "The room isn't taking players right now.") \
    X(why_host_away, "Your host isn't there to answer right now.") \
    X(why_not_listed, "This room isn't in Rooms any more.") \
    X(why_old, "Needs PS5Upload 13.0.0 or newer.") \
    X(why_room_closed, "This room is closed.") \
    X(name_taken_title, "Someone in this room is already called %s.") \
    X(name_join_as, "Join as %s") \
    X(name_pick, "Pick another name") \
    X(st_your_conn, "Your connection: %s · %d ms") \
    X(pw_needed, "This room needs its password.") \
    X(end_removed, "%s removed you from the room.") \
    X(end_room_closed, "%s closed the room.") \
    X(ov_title, "Room") \
    X(ov_you, "You") \
    X(ov_host_link, "Host's PS5") \
    X(ov_quick, "Quick messages") \
    X(ov_type, "Type a message") \
    X(ov_mic, "My mic") \
    X(ov_how_talk, "How I talk") \
    X(talk_ptt, "Push-to-talk (hold Create)") \
    X(talk_open, "Open mic") \
    X(talk_latched, "While my mic is on") \
    X(pm_give_p1, "Give Player 1") \
    X(pm_take_p1, "Take Player 1 back") \
    X(pm_move_to, "Move to Player %d") \
    X(released_note, "You left. The others on this PS5 play on.") \
    X(ov_duck, "Game sound while others talk") \
    X(duck_off, "Off") \
    X(duck_little, "A little quieter") \
    X(duck_more, "Quieter") \
    X(ov_show_chat, "Show chat over the game") \
    X(ov_hide_rude, "Hide rude words") \
    X(ov_people, "People") \
    X(ov_volume, "%d%%") \
    X(ov_muted, "Muted") \
    X(ov_rules, "Rules & responsibility") \
    X(ov_leave, "Leave room") \
    X(mic_hold, "Hold Create to talk") \
    X(mic_sending, "Talking…") \
    X(mic_no_mic, "No mic found") \
    X(mic_muted_sys, "Your mic is muted") \
    X(mic_host_muted, "The host muted your mic") \
    X(mic_room_off, "Voice is off in this room") \
    X(mic_paused, "Voice paused: your connection needs it for the game") \
    X(mic_headset, "Open mic needs a headset here: push-to-talk stays on.") \
    X(mic_off, "Mic off") \
    X(mic_needs_host, "Some room features need the host to update PS5Upload.") \
    X(chat_off, "Chat is off") \
    X(chat_muted, "The host muted chat for you") \
    X(chat_slow, "Slow down") \
    X(chat_quick_only, "Quick messages only in this room") \
    X(chat_new, "%d new") \
    X(chat_line, "%s: %s") \
    X(play_hint_room, "Hold OPTIONS + TOUCHPAD, then press △: the Room menu") \
    X(quick_1, "Ready?") \
    X(quick_2, "Ready!") \
    X(quick_3, "GG") \
    X(quick_4, "One more?") \
    X(quick_5, "Brb") \
    X(quick_6, "Wait") \
    X(quick_7, "Let's go") \
    X(quick_8, "Thanks") \
    X(quick_9, "Nice!") \
    X(quick_10, "Sorry") \
    X(quick_11, "Pause please") \
    X(quick_12, "Lag on my side") \
    X(quick_13, "Can't hear you") \
    X(quick_14, "Switch seats?") \
    X(quick_15, "Bye") \
    X(quick_16, "Thumbs up") \
    X(tab_room, "My room") \
    X(tab_rp, "Remote Play") \
    X(tab_ps5, "This PS5") \
    X(vis_private, "Private") \
    X(vis_friends, "Friends") \
    X(vis_public, "Public") \
    X(vis_private_sub, "Only people you invite") \
    X(vis_friends_sub, "People you've played with see it's open") \
    X(vis_public_sub, "Anyone can find it in Rooms") \
    X(vis_note, "Changing who can see your room never removes anyone.") \
    X(room_head, "%s · %s · %d of %d players") \
    X(listed_ok, "Listed in Rooms") \
    X(listed_listing, "Listing…") \
    X(listed_not, "Not listed: %s") \
    X(nl_internet_off, "Turn on Invites work: Anywhere") \
    X(nl_hub, "Can't reach the rooms service") \
    X(nl_name, "Pick another name") \
    X(nl_busy, "Too many rooms right now, try again later") \
    X(nl_limit, "Your network has too many rooms open") \
    X(nl_old_hub, "This hub has no rooms (self-hosted)") \
    X(r_asks, "Asks") \
    X(r_players, "Players") \
    X(r_room, "Room") \
    X(ask_from, "%s · %s · from %s") \
    X(src_invite, "invite") \
    X(src_again, "Play again") \
    X(src_nearby, "Nearby") \
    X(src_friends, "Friends") \
    X(src_rooms, "Rooms") \
    X(src_group, "the same PS5") \
    X(ask_pw_ok, "password ✓") \
    X(ask_about, "about %d ms") \
    X(ask_group, "%s's PS5: %d players") \
    X(pl_you, "You (%s)") \
    X(pl_from_rooms, "from Rooms") \
    X(pm_mute_voice, "Mute voice") \
    X(pm_unmute_voice, "Unmute voice") \
    X(pm_mute_chat, "Mute chat") \
    X(pm_unmute_chat, "Unmute chat") \
    X(pm_remove, "Remove…") \
    X(rm_title, "Remove %s? They leave the game now.") \
    X(rm_block_room, "Don't let them back in this room") \
    X(rm_block_always, "Block on this computer") \
    X(rm_go, "Remove") \
    X(set_vis, "Who can see it") \
    X(set_password, "Password") \
    X(pw_none, "None") \
    X(pw_on, "On") \
    X(set_size, "Room size") \
    X(size_val, "%d players") \
    X(set_ap_friends, "People you've played with get in without asking") \
    X(set_ap_password, "The password lets people in without asking") \
    X(set_chat, "Chat") \
    X(chat_mode_off, "Off") \
    X(chat_mode_quick, "Quick messages only") \
    X(chat_mode_on, "On") \
    X(set_voice_mode, "Voice") \
    X(voice_off, "Off") \
    X(voice_ptt, "Push-to-talk only") \
    X(voice_open, "Open mic allowed") \
    X(set_start_muted, "New players start muted") \
    X(room_lock, "Lock room") \
    X(room_unlock, "Unlock") \
    X(room_locked, "Locked: no one new can ask") \
    X(room_blocked, "Blocked (%d)") \
    X(room_unblock, "Unblock %s") \
    X(room_close, "Close room") \
    X(close_q, "Close your room? %s leave the game.") \
    X(close_q_empty, "Close your room?") \
    X(keep_open, "Keep it open") \
    X(mute_all, "Mute all") \
    X(unmute_all, "Unmute all") \
    X(public_first, "Anyone using PS5Upload can see this room and ask to join. You still decide who plays. People from the list never get your PS5 user, and PS & Create stay off for them.") \
    X(make_public, "Make public") \
    X(cancel, "Cancel") \
    X(remove_pw_q, "Remove the password? New people won't need it to ask.") \
    X(remove_pw_go, "Remove the password") \
    X(size_below, "%d people are in. Remove someone first.") \
    X(owner_only, "Only %s can do this from this TV.") \
    X(pw_short, "A password needs 4 characters or more.") \
    X(set_room_name, "Room name") \
    X(room_none, "Share your invite, or make the room Friends or Public so people can find it.") \
    X(room_easier, "Easier in PS5Upload › Play › Play Room") \
    X(host_link, "Your PS5 link") \
    X(host_room_closed, "No room open. Start Remote Play in PS5Upload › Play, then open your room here.") \
    X(host_open_room, "Open my room") \
    X(notice_title, "Play Rooms: you're responsible for what you say and do") \
    X(notice_body, "PS5Upload only gives you the tool to play together. We don't run, watch, record or moderate rooms, chat or voice, and we aren't responsible for what anyone says, shares or does in them, including names and room names. You're responsible for your own conduct and for following the law and the rules of the platforms and games you use. Only play with people you trust; you can mute, hide, remove or block anyone.") \
    X(notice_parents, "Parents: please supervise children who use Play Rooms.") \
    X(notice_accept, "I understand") \
    X(notice_later, "Not now") \
    X(thanks_title, "Thank you") \
    X(thanks_count, "%d supporters, and %d more who stayed anonymous") \
    X(thanks_text, "PS5Upload is free. These people help keep it going.") \
    X(thanks_kofi, "Support on Ko-fi") \
    X(thanks_more, "More ways to support") \
    X(thanks_scan, "Scan with your phone.") \
    X(wel_1, "Play a friend's game on this PS5 with your own controller. They stream it from PS5Upload on their computer.") \
    X(wel_2, "Or host: on your own PS5, see who wants to join and share your invite from the couch.") \
    X(wel_3, "What you need: your friend has PS5Upload open with a Play Room, and you're on the same network, on Tailscale, or have their invite on your phone.") \
    X(wel_got_it, "Got it") \
    X(help_title, "Controls") \
    X(help_move, "D-pad or left stick: move. ✕ choose, ○ back, △ help, OPTIONS settings.") \
    X(help_tabs, "L1 / R1: tabs and sorts.") \
    X(help_ps, "In a game, hold OPTIONS + TOUCHPAD for your friend's PS button.") \
    X(help_leave, "Hold L1 + R1 + OPTIONS + TOUCHPAD to leave the game.") \
    X(help_room, "Hold OPTIONS + TOUCHPAD and press △ for the Room menu: chat, mic, volume.") \
    X(help_talk, "With your mic on, hold Create to talk.") \
    X(set_thanks, "Thanks & Support") \
    X(set_how, "How it works") \
    X(set_rules, "Rules & responsibility") \
    X(seats_title, "Players on this PS5") \
    X(seat_joining, "Joining") \
    X(seat_hold, "Hold ✕ to come along") \
    X(seat_asking, "Asking…") \
    X(seat_playing, "Player %d") \
    X(seat_away, "Controller off") \
    X(seat_full, "The game is full (3 friends at most).") \
    X(toast_asking, "%s: asking to join…") \
    X(toast_in, "%s is in the game") \
    X(pair_confirm, "Confirm on %s: does it show this code? Press Pair there.") \
    X(pair_auto, "Pairing through the helper…") \
    X(pair_moved, "%s now uses a new address. Confirm it in PS5Upload.") \
    X(pick_yours, "Your PS5Upload") \
    X(pick_friends, "Friends' games") \
    X(hint_its_me, "It's me") \
    X(ask_own, "%s · your own device") \
    X(ready_check, "Ready check") \
    X(ready_running, "Ready? %d ready · %d s") \
    X(ready_all, "Everyone's ready") \
    X(ready_yes, "Ready") \
    X(ready_banner, "Ready? Answer in the Room menu (hold OPTIONS + TOUCHPAD, then △) · %d s") \
    X(ov_ready, "I'm ready · %d s") \
    X(ready_sent, "You're ready. The host can see it.") \
    X(ov_leave_again, "Press ✕ again to leave the game") \
    X(err_slow, "Too soon: try again in a minute.") \
    X(err_nobody, "Nobody is in your room yet.") \
    X(err_ready_running, "A ready check is already on.") \
    X(err_not_own, "That ask isn't from your own device.") \
    X(host_said_no, "PS5Upload didn't do that (%s).")

enum class S : int
{
#define PLAY_STR_ID(id, text) id,
    PLAY_STRINGS(PLAY_STR_ID)
#undef PLAY_STR_ID
        count_
};

const char *tr(S id) noexcept;
} // namespace play::ui
