//! The curated payload table: which third-party PS5 payloads the Payloads screen and the
//! API know about, and where their releases live. See the module above for the rest.

/// One row in the catalogue. Every field is `&'static` so the table
/// is a `const` — no allocation, no runtime initialisation, ships
/// directly in the .rodata segment.
///
/// Adding a new payload: append a row to `CATALOGUE` below. No code
/// changes elsewhere are required — Phase 3 and Phase 4 read the same
/// rows for detection and autoloader writes.
#[derive(Clone, Copy)]
pub(super) struct CatalogueEntry {
    /// Stable identifier, kebab-case. Used as the on-disk cache
    /// directory name and as the lookup key from the renderer.
    /// Must remain stable across releases — rename → cache miss for
    /// every user.
    pub(super) id: &'static str,
    /// User-facing display name. Shown in the Payloads grid.
    pub(super) display_name: &'static str,
    /// One-line role blurb. Shown under the name.
    pub(super) role: &'static str,
    /// Two-sentence description for the detail card.
    pub(super) description: &'static str,
    /// Where the releases live. `github.com` is the default; any
    /// Forgejo or Gitea server (which expose an API at
    /// `/api/v1/repos/...` with JSON shape identical to GitHub's,
    /// down to the `tag_name` field and asset `name` /
    /// `browser_download_url` fields) is also supported. We
    /// URL-rewrite by host: `github.com` becomes
    /// `https://api.github.com/repos/...`; any other host becomes
    /// `https://{host}/api/v1/repos/...`. 2.13.0 added this to
    /// unlock the earthonion-hosted PS5 payloads (garlic-worker,
    /// np-fake-signin) that aren't mirrored on GitHub.
    pub(super) repo_host: &'static str,
    pub(super) repo_owner: &'static str,
    pub(super) repo_name: &'static str,
    /// Substring matched against each release asset's `name` field
    /// to pick the canonical ELF when multiple are uploaded.
    /// Empty string = "first asset whose name ends in .elf".
    pub(super) asset_name_hint: &'static str,
    /// File the payload writes to /data on first run, used by
    /// `payloads_check_running` for "installed but not yet loaded"
    /// detection. None = no canonical marker, fall back to ports.
    pub(super) on_console_marker_path: Option<&'static str>,
    /// Process name in PROC_LIST output, for "is it running" probe.
    /// None = no reliable process name (some payloads spawn under
    /// different names per build).
    pub(super) process_name_hint: Option<&'static str>,
    /// TCP ports this payload binds when alive. Used for live-probe
    /// detection (Phase 3).
    pub(super) ports: &'static [u16],
    /// Suggested autoload-order index for Phase 4's autoload.txt.
    /// kstuff = 0 (must run first to enable the others), SMP = 1,
    /// ps5upload = 2, ftpsrv etc = 3+.
    pub(super) autoload_priority: u8,
    /// Milliseconds to pause AFTER this payload before loading the
    /// next entry in autoload.txt. kstuff needs ~3s to settle the
    /// kernel patches; userland payloads need much less.
    pub(super) autoload_delay_ms: u32,
    /// Project homepage (UI links to this for "learn more").
    pub(super) homepage: &'static str,
}

/// The curated catalogue. Order here is also the default UI display
/// order — keep the most-load-bearing payloads (kstuff, SMP) at the
/// top so they're visible without scrolling.
///
/// Sources for the entries:
///
///   - kstuff-echostretch: covers FW 1.00 → 12.x via runtime NID
///     resolution; the same binary works on every supported firmware
///     revision. Best default when firmware coverage matters most.
///   - kstuff-lite-drakmor: fork of EchoStretch/kstuff-lite tuned for
///     3-4× faster .ffpkg (UFS) mounting and lower per-mount overhead.
///     Narrower FW range (3.00 → 10.01). Better choice for users whose
///     primary workflow is ShadowMount+ with .ffpkg / .exfat images.
///   - shadowmountplus: the homebrew mount layer most users pair with
///     kstuff for image-based game launching.
///   - ftpsrv / websrv / ps5-app-dumper: utility payloads in common
///     use on the platform.
///   - etahen / itemzflow: best-effort entries; users can edit this
///     table to point at the variant they prefer.
pub(super) const CATALOGUE_HOSTS: &[&str] = &["github.com", "git.etawen.dev", "theghostonline.dev"];
pub(super) const CATALOGUE: &[CatalogueEntry] = &[
    CatalogueEntry {
        id: "elfldr",
        display_name: "elfldr",
        role: "ELF loader — load before ps5upload.elf",
        description: "Initial ELF loader for WebKit/payload-listener setups. Download elfldr-ps5.elf and load it through your jailbreak or autoloader before the PS5Upload helper. Sending requires an already working loader; this cannot bootstrap a closed port. This is the upstream build; PS5Upload can replace the standard running loader with its bundled patched build once the helper is connected.",
        repo_host: "github.com",
        repo_owner: "ps5-payload-dev",
        repo_name: "elfldr",
        asset_name_hint: "elfldr-ps5.elf",
        on_console_marker_path: None,
        process_name_hint: Some("elfldr.elf"),
        ports: &[9021],
        autoload_priority: 1,
        autoload_delay_ms: 1000,
        homepage: "https://github.com/ps5-payload-dev/elfldr",
    },
    CatalogueEntry {
        // display_name says "kstuff-lite (EchoStretch)" so this entry
        // must point at EchoStretch/kstuff-lite, not EchoStretch/kstuff
        // — both repos exist on EchoStretch's account, and the catalog
        // previously linked to the *wrong* one (kstuff, the full
        // build), so users following the Releases / Homepage link
        // landed on a different artifact than the one named here.
        // Fixed 2026-05-27 after a user report.
        id: "kstuff-echostretch",
        display_name: "kstuff-lite (EchoStretch)",
        role: "Kernel exploit + R/W primitive",
        description: "Kernel patcher for the full PS5 firmware range. Resolves kernel symbols at runtime via the SDK's NID table, so the same binary covers FW 1.00 → 12.x. Required by ShadowMountPlus and most other privileged payloads. Load this first.",
        repo_host: "github.com",
        repo_owner: "EchoStretch",
        repo_name: "kstuff-lite",
        asset_name_hint: "kstuff",
        on_console_marker_path: Some("/data/kstuff.elf"),
        process_name_hint: None,
        ports: &[],
        autoload_priority: 0,
        autoload_delay_ms: 3000,
        homepage: "https://github.com/EchoStretch/kstuff-lite",
    },
    CatalogueEntry {
        // Same role as kstuff-echostretch (kernel R/W + ucred elevation
        // + ShadowMount+ enablement) — they're alternatives, not
        // companions. Install only one. Picked over the EchoStretch
        // build when the user's main workflow is mounting .ffpkg /
        // .exfat images: drakmor's tree includes the per-mount fast
        // path and the repeated-operation overhead reduction
        // (autoload pause/resume + remount cycles) that ShadowMount+
        // exercises heavily. Marker path is shared (both drop
        // /data/kstuff.elf) so detection-of-presence is a single
        // probe — but the *active* variant has to be inferred from
        // user choice in the autoload list, not the marker.
        id: "kstuff-lite-drakmor",
        display_name: "kstuff-lite (drakmor — fpkg-optimized)",
        role: "Kernel exploit + R/W primitive — faster .ffpkg/PFS mounting",
        description: "Fork of EchoStretch/kstuff-lite with a hot path for .ffpkg (UFS) + PFS mounts and lower overhead in repeated mount/unmount cycles. Recent builds extended firmware coverage through FW 12.xx (the '12.xx Support' update) on top of the original 3.00→10.01 range — check the release notes for your exact firmware. Adds an option to disable automatic mounting (noautomount) for a controlled startup. Recommended when your primary workflow is ShadowMount+ with .ffpkg/.exfat/PFS images. Same load-first ordering as any other kstuff: must boot before ShadowMount+ or ps5upload.",
        repo_host: "github.com",
        repo_owner: "drakmor",
        repo_name: "kstuff-lite",
        asset_name_hint: "kstuff",
        on_console_marker_path: Some("/data/kstuff.elf"),
        process_name_hint: None,
        ports: &[],
        autoload_priority: 0,
        autoload_delay_ms: 3000,
        homepage: "https://github.com/drakmor/kstuff-lite",
    },
    CatalogueEntry {
        id: "shadowmountplus",
        display_name: "ShadowMount+",
        role: "Auto-mount + register daemon for game backups",
        description: "Watches scan folders (/mnt/usb*, /mnt/ext*, /data/homebrew, /mnt/shadowmnt) AND /data/shadowmount/manual.lst for game folders and .ffpkg/.exfat/.ffpfs/.ffpfsc images, then auto-mounts (LVD/MD), stages sce_sys + appmeta + trophy data, and registers them on the home screen — no per-image command, it's fully autonomous. Newer builds add nested/compressed-PFS (.ffpfsc) containers, trophy-data copy, and a watched manual-install list. Includes fakelib (backports) overlay + per-game kstuff autopause. Needs kstuff-lite v1.07+ loaded first.",
        repo_host: "github.com",
        repo_owner: "drakmor",
        repo_name: "shadowMountPlus",
        asset_name_hint: "shadowmountplus",
        on_console_marker_path: Some("/data/shadowmount/debug.log"),
        process_name_hint: Some("shadowmountplus"),
        ports: &[],
        autoload_priority: 1,
        autoload_delay_ms: 1000,
        homepage: "https://github.com/drakmor/shadowMountPlus",
    },
    CatalogueEntry {
        id: "etahen",
        display_name: "etaHEN",
        role: "Homebrew enabler + jailbreak helper",
        description: "Long-running homebrew enabler with toolbox features. Faster app jailbreak than the on-the-fly path; provides the HijackerCommand IPC many homebrew apps expect on :9028.",
        repo_host: "github.com",
        repo_owner: "LightningMods",
        repo_name: "etaHEN",
        asset_name_hint: "etaHEN",
        on_console_marker_path: None,
        process_name_hint: Some("etaHEN"),
        ports: &[9028, 2323],
        autoload_priority: 2,
        autoload_delay_ms: 500,
        homepage: "https://github.com/LightningMods/etaHEN",
    },
    CatalogueEntry {
        id: "ftpsrv",
        display_name: "ftpsrv",
        role: "FTP server payload",
        description: "Lightweight FTP server on :2121 with SELF↔ELF auto-decryption and remount-RW SITE commands. Lets you browse the PS5's filesystem from any FTP client.",
        repo_host: "github.com",
        repo_owner: "ps5-payload-dev",
        repo_name: "ftpsrv",
        asset_name_hint: "ftpsrv",
        on_console_marker_path: None,
        process_name_hint: Some("ftpsrv"),
        ports: &[2121],
        autoload_priority: 3,
        autoload_delay_ms: 200,
        homepage: "https://github.com/ps5-payload-dev/ftpsrv",
    },
    CatalogueEntry {
        id: "websrv",
        display_name: "websrv",
        role: "Web-based homebrew launcher",
        description: "HTTP server on :8080 serving a homebrew launcher page. Pairs with the homebrew bundles distributed by ps5-payload-dev.",
        repo_host: "github.com",
        repo_owner: "ps5-payload-dev",
        repo_name: "websrv",
        asset_name_hint: "websrv",
        on_console_marker_path: None,
        process_name_hint: Some("websrv"),
        ports: &[8080],
        autoload_priority: 3,
        autoload_delay_ms: 200,
        homepage: "https://github.com/ps5-payload-dev/websrv",
    },
    CatalogueEntry {
        id: "ps5-app-dumper",
        display_name: "ps5-app-dumper",
        role: "Dump installed games to a drive",
        description: "Dumps the running game, decrypted, to a USB drive or the console, one at a time or as a queue. ps5upload drives it from Games → Dumps (enter the code the TV shows once); it also has a web page of its own on port 8081.",
        repo_host: "github.com",
        // The releases live under EchoStretch, not ps5-payload-dev (the old
        // owner 404s — GitHub issue #82). EchoStretch/ps5-app-dumper ships
        // ps5-app-dumper.elf on each tag.
        repo_owner: "EchoStretch",
        repo_name: "ps5-app-dumper",
        asset_name_hint: "dumper",
        on_console_marker_path: None,
        process_name_hint: Some("dumper"),
        ports: &[8081],
        autoload_priority: 4,
        autoload_delay_ms: 200,
        homepage: "https://github.com/EchoStretch/ps5-app-dumper",
    },
    CatalogueEntry {
        id: "itemzflow",
        display_name: "Itemzflow",
        role: "PS5 native homebrew launcher UI",
        description: "Full-screen native PS5 launcher for homebrew, fpkg games, and FTP browsing. Heavyweight (~50 MB) but the most polished launcher in the scene.",
        repo_host: "github.com",
        // The repo is LightningMods/Itemzflow — "itemzflow_PS5" 404s (GitHub
        // issue #82). Note Itemzflow is distributed as a .pkg app, so its GitHub
        // releases may not always carry a downloadable payload asset; resolving
        // the repo at least replaces the hard 404 with the real release list.
        repo_owner: "LightningMods",
        repo_name: "Itemzflow",
        asset_name_hint: "itemzflow",
        on_console_marker_path: None,
        process_name_hint: Some("itemzflow"),
        ports: &[],
        autoload_priority: 5,
        autoload_delay_ms: 200,
        homepage: "https://github.com/LightningMods/Itemzflow",
    },
    CatalogueEntry {
        // Requested by users. Loads game cheats on-console; pairs with
        // a cheat database the user manages on the PS5 side.
        id: "cheatrunner",
        display_name: "CheatRunner",
        role: "Game cheat loader",
        description: "Loads and applies game cheats on the PS5. Send it like any other payload, then browse and toggle cheats for supported titles on the console.",
        repo_host: "github.com",
        repo_owner: "notmaj0r",
        repo_name: "CheatRunner",
        asset_name_hint: "",
        on_console_marker_path: None,
        process_name_hint: None,
        ports: &[],
        autoload_priority: 6,
        autoload_delay_ms: 200,
        homepage: "https://github.com/notmaj0r/CheatRunner",
    },
    CatalogueEntry {
        // Companion to our Shell tab. Pre-2.13.0 we shipped 17
        // in-payload built-ins; 2.13.0 raised that to 42 (parity
        // with shsrv). shsrv still offers two things our built-
        // ins don't: (a) `hbldr` — full ELF launcher with audio +
        // video, useful for running standalone homebrew apps with
        // graphical output; (b) `hbdbg` — gdb-style debug shell.
        // Both rely on the SpZeroConf injection chain
        // (elfldr.c:74,593) which we don't currently implement in
        // our payload. Add as a one-click companion for users who
        // want those two features. GPLv3.
        id: "shsrv",
        display_name: "shsrv (telnet shell + ELF launcher + gdb)",
        role: "42-command telnet shell + hbldr + hbdbg",
        description: "Telnet server on :2323 with 42 POSIX-ish commands (sfoinfo, file, hexdump, find with -exec, etc.) plus hbldr (launch unsigned ELF with full A/V) and hbdbg (gdb-style debugger). Our Shell tab covers the same 42 built-ins over the paired AVA1 session; install shsrv if you want hbldr/hbdbg or you prefer telnet access. Connect via `telnet <ps5-ip> 2323`.",
        repo_host: "github.com",
        repo_owner: "ps5-payload-dev",
        repo_name: "shsrv",
        asset_name_hint: "shsrv",
        on_console_marker_path: None,
        process_name_hint: Some("shsrv.elf"),
        ports: &[2323],
        autoload_priority: 4,
        autoload_delay_ms: 200,
        homepage: "https://github.com/ps5-payload-dev/shsrv",
    },
    // NOTE: the "lapyjb" (Lapy JB Daemon, voidwhisper) entry was removed in
    // 3.3.25 — its upstream `git.etawen.dev/voidwhisper/lapy-jb-daemon` 404s
    // (repo gone), and no live replacement exists (the community fork
    // `ArkSama/PS5-Lapy-JB-Daemon` ships zero releases). It was the only dead
    // source in the catalogue — the other git.etawen.dev entries
    // (np-fake-signin, garlic-worker, garlic-savemgr) resolve fine. Re-add it
    // if a maintained release home reappears. See GitHub issue #82.
    CatalogueEntry {
        // Offline account activation — registers user slots in
        // the PS5 settings registry without signing into PSN.
        // Useful for fresh-jailbreak setups where the user
        // wants to play homebrew/backups without any PSN
        // account. Sonic Loader uses earthonion's fork that
        // dropped the SDL2 UI for headless registry writes.
        // ps5-webkit-autoloader — installs an autoloader into the
        // console's browser so payloads load from the WebKit entry
        // point without a PC in the loop each time.
        //
        // The release ships three assets: a Windows host .exe, a
        // Python host .py, and the on-console installer .elf. Only the
        // .elf is a payload; pick_asset skips the other two because
        // they have no payload-shaped extension, but the hint names
        // the installer explicitly so the intent is legible rather
        // than relying on that filter.
        //
        // Asset names carry the version ("..._v0.3.1.elf"), so the
        // hint deliberately stops before the version — a pinned hint
        // would stop matching at the next release.
        id: "webkit-autoloader",
        // Named for what it gives the user; the upstream name stays in brackets. Not
        // to be confused with the USB payload stick or the playlist on connect.
        display_name: "Browser payload menu (WebKit Autoloader installer)",
        role: "Loads payloads from the console's browser",
        description: "Installs a payload menu in the PS5's own web browser, so payloads can be launched from the console without sending them from a PC every time. One-shot installer ELF: send it, it installs, it exits. Run it after your kernel exploit, like any other payload.",
        repo_host: "github.com",
        repo_owner: "itsPLK",
        repo_name: "ps5-webkit-autoloader",
        asset_name_hint: "webkit-autoloader-installer",
        on_console_marker_path: None,
        process_name_hint: None,
        ports: &[],
        autoload_priority: 5,
        autoload_delay_ms: 200,
        homepage: "https://github.com/itsPLK/ps5-webkit-autoloader",
    },
    CatalogueEntry {
        // itsPLK's Payload Manager: keeps payloads on the console
        // (/data/pldmgr), launches them from its own web page on :8084 and
        // at boot. ps5upload's Payloads → Payload Manager tab talks to it,
        // and the catalogue's "Store in Payload Manager" fills it.
        id: "payload-manager",
        display_name: "Payload Manager (itsPLK)",
        role: "Keeps payloads on the console; loads them at boot",
        description: "Stores payloads on the PS5 itself and starts the ones you mark when the console boots, so a USB stick or a PC is not needed after setup. Has its own web page on port 8084. ps5upload's Payload Manager tab lists what it holds, launches or deletes entries, and edits the boot list; the Catalog's \"Store in Payload Manager\" keeps a payload there.",
        repo_host: "github.com",
        repo_owner: "itsPLK",
        repo_name: "ps5-payload-manager",
        asset_name_hint: "pldmgr",
        on_console_marker_path: Some("/data/pldmgr/pldmgr_config.txt"),
        process_name_hint: None,
        ports: &[8084],
        autoload_priority: 2,
        autoload_delay_ms: 1000,
        homepage: "https://github.com/itsPLK/ps5-payload-manager",
    },
    CatalogueEntry {
        // theghostonline's LZ4 Compression: packs an installed game's AMPR
        // assets with LZ4 on the console itself (no PC round trip), the
        // same pack format ps5upload's Convert writes on the PC. Web page
        // on :5920; the Connection screen's scene-tools strip opens it.
        id: "lz4-compression",
        display_name: "LZ4 Compression (theghostonline)",
        role: "Compresses games on the PS5 itself",
        description: "Shrinks a game that is already on the PS5 by LZ4-packing its AMPR assets, entirely on the console: the game never leaves it. Works on folder games and on mounted .exfat/.ffpfs images, packs in place with rollback, verifies every file, and can undo. Needs ShadowMount+ for image sources. Open its page on port 5920 from the Connection screen's scene tools once it is running. ps5upload's Convert does the same packing on this computer when the game is here instead.",
        repo_host: "theghostonline.dev",
        repo_owner: "theghostonline",
        repo_name: "PS5-LZ4-Compression",
        asset_name_hint: "lz4-compression",
        on_console_marker_path: Some("/data/LZ4Compression"),
        process_name_hint: None,
        ports: &[5920],
        autoload_priority: 9,
        autoload_delay_ms: 500,
        homepage: "https://theghostonline.dev/theghostonline/PS5-LZ4-Compression",
    },
    CatalogueEntry {
        id: "np-fake-signin",
        display_name: "NP Fake Sign-in",
        role: "Fake PSN sign-in: unlocks the PS5's Remote Play setting",
        description: "Makes the console treat an activated local account as signed in to PlayStation Network, with no real PSN sign-in. That is what lets you turn on Settings → System → Remote Play. Use v1.4 or later. The account must already be activated (offline activation). One-shot ELF: send it, it runs and exits; restart the PS5 afterwards. To undo: Settings → Users and Accounts → Other → Sign out.",
        repo_host: "git.etawen.dev",
        repo_owner: "earthonion",
        repo_name: "np-fake-signin",
        // Releases ship BOTH np-fake-signin-ps5.elf and -ps4.elf; the
        // "-ps5" suffix is a substring of only the PS5 asset, so pick_asset
        // selects the right build (same trick as nanoDNS below).
        asset_name_hint: "np-fake-signin-ps5",
        on_console_marker_path: None,
        process_name_hint: None,
        ports: &[],
        autoload_priority: 5,
        autoload_delay_ms: 200,
        homepage: "https://git.etawen.dev/earthonion/np-fake-signin",
    },
    CatalogueEntry {
        // Garlic Worker — community save-decryption queue
        // worker that processes encrypted PS4/PS5 save files
        // for users who request them via garlicsaves.com.
        // **Privacy-sensitive**: connects out to a community
        // server. Catalog entry only; users opt in by
        // installing + running. Sonic Loader has this enabled
        // by default; we keep it off-by-default to honour the
        // no-telemetry posture.
        id: "garlic-worker",
        display_name: "Garlic Worker (community save processor)",
        role: "Process community save decryption jobs (opt-in)",
        description: "Background worker that drains the community save-decryption queue from garlicsaves.com. Handles both PS4 and PS5 saves natively. **Privacy notice**: connects to garlicsaves.com and processes other users' encrypted save files. Off by default — install + run manually if you want to contribute back to the community queue.",
        repo_host: "git.etawen.dev",
        repo_owner: "earthonion",
        repo_name: "garlic-worker",
        asset_name_hint: "garlic-worker-ps5",
        on_console_marker_path: None,
        process_name_hint: Some("garlic-worker"),
        ports: &[],
        autoload_priority: 6,
        autoload_delay_ms: 200,
        homepage: "https://git.etawen.dev/earthonion/garlic-worker",
    },
    CatalogueEntry {
        // Garlic SaveMgr — decrypts and re-encrypts YOUR OWN
        // save files locally on the console (no network). Two
        // distinct daemons: savemgr (decrypt yours) is what
        // most users want; worker (community queue) is the
        // opt-in cooperative bit. Sonic Loader bundles both;
        // we catalog both separately for clarity.
        id: "garlic-savemgr",
        display_name: "Garlic SaveMgr (decrypt your own saves)",
        role: "Decrypt + re-encrypt your own PS5/PS4 saves",
        description: "On-console save decrypt/encrypt daemon. Lets you back up saves in plaintext, edit them on PC, and re-encrypt for the same console. No network — operates purely on saves you already own. Companion to ps5upload's Saves tab; install this for round-trip plaintext editing workflows.",
        repo_host: "git.etawen.dev",
        repo_owner: "earthonion",
        repo_name: "garlic-savemgr",
        asset_name_hint: "garlic-savemgr",
        on_console_marker_path: None,
        process_name_hint: Some("garlic-savemgr"),
        ports: &[],
        autoload_priority: 5,
        autoload_delay_ms: 200,
        homepage: "https://git.etawen.dev/earthonion/garlic-savemgr",
    },
    CatalogueEntry {
        // Companion to /logs?tab=kernel for users who want
        // persistent on-console klog capture across desktop-app
        // restarts. ps5upload's own KLOG_READ already streams
        // /dev/klog into the Logs tab — klogsrv adds rotated
        // file persistence at /data/klog/klog.log (10 backups)
        // and a separate `nc <ip> 3232` netcat endpoint. Niche;
        // most users won't need it. GPLv3, can't bundle — we
        // reference the upstream's GitHub release.
        id: "klogsrv",
        display_name: "klogsrv",
        role: "Persistent /dev/klog netcat server + rotated log",
        description: "Streams /dev/klog over TCP :3232 and tees it to /data/klog/klog.log (10-backup rotation). Useful for capturing kernel-log activity that happens while the ps5upload desktop app is closed, or for tailing klog via plain netcat without our payload.",
        repo_host: "github.com",
        repo_owner: "ps5-payload-dev",
        repo_name: "klogsrv",
        asset_name_hint: "klogsrv",
        on_console_marker_path: Some("/data/klog/klog.log"),
        process_name_hint: Some("klogsrv.elf"),
        ports: &[3232],
        autoload_priority: 4,
        autoload_delay_ms: 200,
        homepage: "https://github.com/ps5-payload-dev/klogsrv",
    },
    CatalogueEntry {
        // drakmor/nanoDNS — a tiny DNS server that runs ON the console.
        // Blocks PlayStation Network / update domains by default (maps
        // them to 0.0.0.0) and can redirect any domain to a LAN IP. The
        // user points the PS5's DNS at it (default bind 127.0.0.1 =
        // console-local; set bind=0.0.0.0 in the ini to serve the LAN).
        // Config is auto-created at /data/nanodns/nanodns.ini on first run.
        //
        // CRITICAL asset pick: the release ships BOTH `nanodns.elf` (PS5)
        // and `nanodns-ps4.elf` (PS4) as separate assets. The hint
        // `nanodns.elf` is a substring of "nanodns.elf" but NOT of
        // "nanodns-ps4.elf" (which reads "...-ps4.elf"), so pick_asset
        // selects the PS5 build. Releases are PRE-RELEASE, so they only
        // show via the releases-list path (payloads_releases), not
        // /releases/latest.
        id: "nanodns",
        display_name: "nanoDNS",
        role: "On-console DNS server — block PSN / redirect domains",
        description: "A minimal DNS server that runs on the PS5 (UDP :53). Ships blocking PlayStation Network + update domains by default (0.0.0.0), and can redirect any domain to a LAN IP — handy for staying offline-friendly while jailbroken. Supports nanoDNS 0.3 and 0.4; the nanoDNS editor detects the running version and preserves existing /data/nanodns/nanodns.ini settings when enabling 0.4 quiet mode or IPv6. Point the console's DNS at it (set bind=0.0.0.0 in the ini to serve the LAN). PS5 build only — never the -ps4 asset.",
        repo_host: "github.com",
        repo_owner: "drakmor",
        repo_name: "nanoDNS",
        asset_name_hint: "nanodns.elf",
        on_console_marker_path: Some("/data/nanodns/nanodns.ini"),
        process_name_hint: Some("nanodns.elf"),
        ports: &[53],
        autoload_priority: 4,
        autoload_delay_ms: 300,
        homepage: "https://github.com/drakmor/nanoDNS",
    },
    CatalogueEntry {
        // BestPig/BackPork — step 3 of backporting. Watches for game
        // launches and union-mounts <title>/fakelib over the sandbox's
        // common/lib, so a game can run against libraries from a newer
        // firmware. Pairs with our SDK Version Changer, which does step 2
        // (downgrading the SDK version recorded in eboot/prx); neither
        // half boots a title on its own. Release 0.1 ships
        // `ps5-backpork.elf`, so the hint matches only the payload.
        id: "backpork",
        display_name: "BackPork (backport helper)",
        role: "Mount replacement system libraries into a game at launch",
        description: "Runs in the background and watches for game launches. When a title starts, it looks for a `fakelib` folder inside that game's install directory and union-mounts it over the sandbox's `common/lib`, so the game loads newer system libraries without altering the originals. This is the runtime half of backporting a game to older firmware — use it together with the SDK Version Changer, which lowers the SDK version recorded in `eboot.bin` and every `prx`/`sprx`. A title needs both: downgraded ELFs so the system agrees to launch it, and compatible libraries so it does not crash on missing imports. The libraries themselves usually need patching to drop imports your firmware lacks; upstream ships BPS patch sets for 6.xx and 7.xx.",
        repo_host: "github.com",
        repo_owner: "BestPig",
        repo_name: "BackPork",
        asset_name_hint: "ps5-backpork.elf",
        on_console_marker_path: None,
        process_name_hint: Some("ps5-backpork.elf"),
        ports: &[],
        autoload_priority: 6,
        autoload_delay_ms: 300,
        homepage: "https://github.com/BestPig/BackPork",
    },
    CatalogueEntry {
        // StonedModder/Ghostpad — creates a virtual PS5 controller and
        // redirects input to it. GitHub-hosted; releases ship `ghostpad.elf`
        // (the on-console payload) alongside a Windows companion `.exe`. The
        // `ghostpad.elf` hint is a substring of only the ELF, so pick_asset
        // never selects the .exe (which isn't a payload extension anyway).
        id: "ghostpad",
        display_name: "Ghostpad",
        role: "Virtual controller + input redirection",
        description: "Creates a virtual PS5 controller on the console and redirects input to it — useful for input automation, remote control, and accessibility setups. Send the payload to start it; pair it with the upstream's companion app for driving the virtual pad.",
        repo_host: "github.com",
        repo_owner: "StonedModder",
        repo_name: "Ghostpad",
        asset_name_hint: "ghostpad.elf",
        on_console_marker_path: None,
        process_name_hint: Some("ghostpad.elf"),
        ports: &[],
        autoload_priority: 4,
        autoload_delay_ms: 200,
        homepage: "https://github.com/StonedModder/Ghostpad",
    },
    CatalogueEntry {
        // X-F1REBALL-X/HearBridge-PS5 (GPL-3) — plays the console's audio on ordinary
        // Bluetooth (A2DP) headphones or speakers through the PS5's own radio, no dongle.
        // Releases ship one asset, `HearBridge-<version>.elf`, so the hint stops before the
        // version. Its own web page (pairing, volume, latency, EQ) is on TCP 8090, or the next
        // free port up to 8095; /data/hearbridge/ holds its state.
        id: "hearbridge",
        display_name: "HearBridge",
        role: "Bluetooth headphones and speakers for PS5 audio",
        description: "Sends the PS5's game and system audio to ordinary Bluetooth headphones or speakers (A2DP), using the console's own Bluetooth radio, so no dongle is needed. Pair, switch headsets, set volume, latency, EQ and night mode, and keep per-game profiles from its own web page (port 8090), which also adds a HearBridge tile to the home screen. Tested by its author on the original PS5 (CFI-10xx) and the Slim (CFI-2000).",
        repo_host: "github.com",
        repo_owner: "X-F1REBALL-X",
        repo_name: "HearBridge-PS5",
        asset_name_hint: "HearBridge-",
        on_console_marker_path: Some("/data/hearbridge/hearbridge.log"),
        process_name_hint: Some("hearbridge.elf"),
        ports: &[8090],
        autoload_priority: 5,
        autoload_delay_ms: 300,
        homepage: "https://github.com/X-F1REBALL-X/HearBridge-PS5",
    },
    CatalogueEntry {
        // disco-trooper/hush (GPL-3) — turns off the console's continuous "recent gameplay"
        // recording by pointing SceAvCapture's sceVencStartEncode import at a stub. Remote
        // Play encodes through the same call, so while Hush is on a stream gets no picture
        // (#561): Play pauses it (enabled=0 in /data/hush/config.ini) and turns it back on.
        // Releases ship `hush.elf` and a `SHA256SUMS` the download is checked against
        // (CHECKSUM_FILES). Hush does not rename its process (elfldr calls it payload.elf),
        // so the client tells it running by the pid in /data/hush/pid.lock.
        id: "hush",
        display_name: "Hush",
        role: "Turn off the console's background gameplay recording",
        description: "Stops the PS5 from continuously recording the last minutes of gameplay to its internal drive (several GB written per hour of play). The Create menu then answers \"Gameplay isn't being recorded\". Settings live in /data/hush/config.ini; enabled=0 restores recording within about 20 seconds. Remote Play uses the same video encoder, so while Hush is on a stream has no picture: Play can pause Hush for the stream and turn it back on afterwards. By disco-trooper.",
        repo_host: "github.com",
        repo_owner: "disco-trooper",
        repo_name: "hush",
        asset_name_hint: "hush.elf",
        on_console_marker_path: Some("/data/hush/config.ini"),
        process_name_hint: Some("hush.elf"),
        ports: &[],
        autoload_priority: 5,
        autoload_delay_ms: 300,
        homepage: "https://github.com/disco-trooper/hush",
    },
    CatalogueEntry {
        // earthonion/remoteplay_unlock (GPL-3) — patches SceRemotePlay
        // (NPXS40102) in memory so a game's Remote Play prohibition (the
        // initial RP-Prohibit answer, prohibition notices mid-stream, the
        // audio/video protection state that blacks out the picture) stops
        // blocking a stream. One-shot: it checks the original bytes before
        // every write (refusing unknown firmware itself), patches, shows a
        // notification and exits — no process, port or marker, so the hub
        // never shows it as running. Releases are a rolling PRE-release
        // tagged `latest` (CI on every push): payloads_release falls back to
        // the newest list entry when /releases/latest 404s. It needs the
        // console's Remote Play service up, so it goes late in autoload.
        id: "remoteplay-unlock",
        display_name: "Remote Play Unlock",
        role: "Let Remote Play stream games that block it",
        description: "Patches the PS5's Remote Play service so games and apps that prohibit Remote Play (\"can't be streamed\", a prohibition notice during a stream, or a black picture) stream normally. Firmware 3.20, 10.01, 12.00, 12.70 and 13.60; it checks the console's code before changing it and refuses other firmware. One-shot: it applies the patch, shows a notification and exits. In memory: load it again after every reboot, or add it to your autoloader (Payload Manager: Load at boot). By earthonion.",
        repo_host: "github.com",
        repo_owner: "earthonion",
        repo_name: "remoteplay_unlock",
        asset_name_hint: "remoteplay_unlock.elf",
        on_console_marker_path: None,
        process_name_hint: None,
        ports: &[],
        autoload_priority: 6,
        autoload_delay_ms: 500,
        homepage: "https://github.com/earthonion/remoteplay_unlock",
    },
    CatalogueEntry {
        // Pharaoh2k/ps5debug-NG — a debugger for jailbroken PS5s (GPL-3). The
        // payload is an installer: it loads the debugger into the console's own
        // SceShellCore process and exits, so there is no process of its own to
        // look for; the command port (744) is how "running" is told. Release
        // assets carry the version (`ps5debug-NG_v1.3.2.elf`), so the hint stops
        // before it. It is a developer tool, never part of a default bring-up:
        // last in any autoload order.
        id: "ps5debug-ng",
        display_name: "ps5debug-NG",
        role: "Debugger: inspect and change a running game's memory",
        description: "A debugger that runs inside the PS5's system process. Tools on your computer (trainers, cheat finders, the ps5dbg Python client) connect to it to read and write a game's memory, scan for values, and set breakpoints. For developers and cheat makers; ps5upload does not need it. While it is attached to a game, ps5upload's own Cheats may not apply to that game.",
        repo_host: "github.com",
        repo_owner: "Pharaoh2k",
        repo_name: "ps5debug-NG",
        asset_name_hint: "ps5debug-ng",
        on_console_marker_path: None,
        process_name_hint: None,
        ports: &[744],
        autoload_priority: 9,
        autoload_delay_ms: 500,
        homepage: "https://github.com/Pharaoh2k/ps5debug-NG",
    },
];
