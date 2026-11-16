# Security Policy

## Supported versions

ps5upload ships from `main` as a rolling release; only the **latest
release** receives fixes. Please reproduce any issue on the most recent
[release](https://github.com/phantomptr/ps5upload/releases) before
reporting.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately via GitHub's **[Report a
vulnerability](https://github.com/phantomptr/ps5upload/security/advisories/new)**
button (Security tab → Report a vulnerability). This opens a private
advisory only you and the maintainer can see.

If you can't use that, reach the maintainer on
[Discord](https://discord.gg/fzK3xddtrM) and ask for a private channel —
don't post details publicly.

Please include:

- what the issue is and its impact,
- steps to reproduce (or a proof of concept),
- affected version / platform.

You'll get an acknowledgement as soon as practical. This is a free,
volunteer-maintained project, so timelines are best-effort — but security
reports are taken seriously and prioritized.

## Scope

ps5upload talks to a **jailbroken PS5 over a LAN**, and it trusts that LAN.

**The helper on the PS5.** AVA1 (port 9120) uses the Noise XX handshake and
encrypts and authenticates every byte. By default it trusts on first connect:
any device that reaches port 9120 is let in, and the console remembers it. A
device needs a code shown on the TV only when **Ask before a new device can
connect** is on, and a blocked device is refused. So by default, anyone on the
network can read, write and delete on the console through the helper. Weaknesses
in the handshake, in approval and blocking, or in AVA1's message parsers are in
scope. The helper also serves PS5Upload Minion on port 9180: to the PS5's own
browser, and to other devices once **Allow other devices on the network** is on.

**The engine.** The engine listens on every interface (`0.0.0.0:19113`), in the
desktop app, in Docker and in the Android and iPhone apps, so the PS5 can fetch
packages from it. A guard answers only loopback callers and addresses listed in
`PS5UPLOAD_ALLOW_IP`. Four things are open to any peer, and each checks for
itself:

- `/pkg-host/`: packages for the PS5, each behind its own unguessable address.
- `/pad` and `/api/pad/ws`: a phone as a controller. The socket checks the QR
  code's key or waits for the computer to allow it.
- `/join` and `/api/join/ws`: a friend joining Play Together. The socket checks
  the invite or the resume token.

A way past that guard or past those checks is in scope. The engine API has no
sign-in of its own: anyone the guard lets through can use all of it.

Reports most in scope:

- the desktop and phone apps and the local engine service,
- the PS5 payload's network-facing handlers,
- the release/build pipeline and bundled dependencies.

Out of scope: an attack that only uses what the defaults above already allow to
anyone on your LAN, physical access to an unlocked machine, and the security of
the third-party jailbreak/loader the payload depends on.
