# AVA1 evaluation 007: pre-hardware work order status

Mirrors `docs-research/007-ava1-e2208e6/05-work-order.md`, worked on branch p3-r007 (from ava1 282fa0b3).
DONE = landed with the commit cited; OPEN = not done, with the reason.

| # | item | status | commit / note |
|---|---|---|---|
| 1 | Pro outage triage (02 §2) | OPEN | Needs the console (`/data/ps5upload/stderr.log`, the crash notice). The levers it asks for first, #4 and #5, are in. |
| 2 | Reconcile CUTOVER §3 with AVA1-only code | DONE | 6b44532e: option (a), no release from this branch until the §3 gates are green; the last FTX2 release stays shipped. |
| 3 | Nonce audit remaining steps | DONE | Ceiling guard, comment, lockstep test were in 282fa0b3 (`AUDIT-nonce.md` §8). The C layout note was stated but not pinned: 6ae8633e states it explicitly and adds `aead.rs::the_c_nonce_is_four_zero_bytes_then_the_counter_little_endian`. |
| 4 | `same_device` fails closed (HW-1) | DONE | 886cbae7: `commit_large` and `finish` refuse on -1 with `ERR_IO`; `fs.rename`, `FS_MOVE` and FTP RNTO refuse `XDEV_UNKNOWN` (`xdev_rename_is_safe`); shell `mv` and the delete walker already refused. Tests: `apply.rs` (-1 and 0, file and tree, nothing renamed), `mgmt_fs.rs` (unreadable devices move nothing, plus a lint), `cross_device_selftest.c`. SPEC §7.3/§11.6 say unknown is refused. |
| 5 | Runtime durable-by-log off-switch (HW-3) | DONE | ad112bff: `/data/ps5upload/debug/ava1-log-small-off`, read once per job at create, one stderr line when off, recovery ignores it. Tests in `log_small.rs` (flag on/off, toggled between jobs, mid-job flip, recovery with the flag set). CUTOVER §4.3 and the new §4.2.1 name the file. |
| 6 | Receiver progress watchdog | DONE | 282fa0b3 (SPEC §12.8), unchanged. |
| 7 | SPEC §11.5 vs console `Resume` credit (C-1) | DONE | a74c6cc2: the code was already right (0074a944 re-sends the grant as `Credit`; the review read an older line). The SPEC now names the console/engine difference (grant minus held vs full grant) and the tests that pin it (`wire_upload::a_resume_after_a_dropped_session_sends_credit_and_the_job_completes`, `data_rust` Resume). The stale CUTOVER bullet is removed. |
| 8 | `sweep_one` first-pass content check | DONE | fd6d47a4. The review's reasoning held. Its test exposed a real bug: the sync batch sorted ids but not their pack locations, so the sweep queue paired ids with other files' records and a live re-make failed with `EIO`; fixed in the same commit. |

Still gated on hardware (not in this order): the Pro outage explanation, the §4 hardware pass on both consoles,
the Opus re-review, and the workspace gate on the release commit.
