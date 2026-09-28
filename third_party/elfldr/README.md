# elfldr — vendored, with one fix

Upstream: <https://github.com/ps5-payload-dev/elfldr> (GPLv3+, John Törnblom),
at commit `02cfe91` ("uri_get_filename: sanity check uri"). The sources here
are upstream's plus `ps5upload.patch`; `elfldr-ps5.elf` is built from them.

ps5upload sends `elfldr-ps5.elf` to a console's loader port (:9021) to replace
the standard `elfldr.elf` running there, once per elfldr instance, so the
console keeps a loader that cannot get stuck (see below).

## The change

elfldr serves one connection at a time, and its reads had no deadline. A
client that connected and then went silent without closing — a dropped
Wi-Fi link, a computer that went to sleep mid-send, a console entering rest
mode while a payload was being sent — blocked every later connection for
good: `elfldr.elf` stayed in the process list but never answered on :9021
again, and users had to load elfldr again before ps5upload.

`ps5upload.patch` gives each accepted connection a 15-second receive deadline
(`SO_RCVTIMEO`) and clears it before a payload inherits the connection as its
stdio. Measured on a PS5 (FW 5.10): with a silent connection held open, the
original answered nothing until it closed; the patched build dropped it after
15.0 s and answered the next request at once. A payload sent in three parts
with 10-second pauses still loads.

## Rebuilding

With the ps5-payload-sdk installed (and `xxd` on the path):

```console
make elfldr        # from the repo root
```

The build stamps its time into elfldr's startup log (`__DATE__`/`__TIME__`), so
a rebuild differs from the checked-in ELF in those bytes only.
