# BearSSL (vendored)

Upstream: https://www.bearssl.org/ (git https://www.bearssl.org/git/BearSSL), commit
7bea48e5e850ab4cafbe68d3765cdaba13a86d6f (v0.6-39-g7bea48e), MIT licence (LICENSE.txt).

Kept in this repository instead of as a git submodule so builds and source downloads never depend
on the upstream server being up. Unchanged except that `T0Comp.exe` (a prebuilt tool that only
regenerates sources already included here) is left out.
