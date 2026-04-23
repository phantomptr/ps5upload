# Payload2

This directory is reserved for the `ps5upload 2.0` PS5 payload rewrite.

Planned responsibilities:

- singleton runtime ownership
- takeover and forced replacement handling
- `FTX2` control and data channels
- durable transaction journal
- bounded writer pipeline
- structured logs, metrics, and crash context

Planned initial files:

- `Makefile`
- `include/config.h`
- `include/runtime.h`
- `src/main.c`
- `src/runtime.c`
- `src/takeover.c`
