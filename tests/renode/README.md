# Renode test infrastructure

Tests for sigchat running on cycle-accurate RV32 emulation via Renode and
Robot Framework.

## Workflow source

The Robot Framework + Renode pattern for testing Xous comes from Antmicro's
[renode-xous-precursor](https://github.com/antmicro/renode-xous-precursor)
(Apache-2.0). Antmicro's `xous.robot` provides the test machine setup and
UART line-matching keywords used here. The robot file in this directory
extends that pattern with sigchat-specific scenarios.

## Prerequisites

- **Renode v1.16.1 or later.** A peripheral API incompatibility (Renode
  1.16's `LimitTimer` requires `ulong` frequency; xous-core's
  `LiteX_Timer_32.cs` passed `long`) was resolved on the project's
  pinned branch in `tunnell/xous-core` PR #18 (issue #13). The fix is
  a one-line parameter-type change and is carried locally per the
  `tunnell/*` policy in `AGENTS.md`.
- **Robot Framework** with the Renode keywords resource. Antmicro's repo
  ships a `tests.sh` that wires this up.
- **A Xous image with sigchat included.** Built via `cargo xtask
  renode-image sigchat:...` from a sibling xous-core checkout.

## Known limitations

- **`tools/measure-renode.sh` smoke test exits 2 (skip), not 0.**
  After the LiteX_Timer fix landed, Renode now compiles all
  peripherals and starts both `SoC` and `EC` machines successfully.
  But the script greps for `INFO:xous_signal_client` log lines that
  never appear because `renode --console --disable-gui` does not
  auto-redirect peripheral UART output. The .repl exposes three
  UARTs (`uart`, `console`, `app_uart`); identifying which one
  carries the kernel logger output and adding
  `sysbus.<name> CreateFileBackend ...` to the boot recipe is open
  follow-up work tracked as issue #34. Robot Framework-based tests
  (`pddb-format.robot`) handle UART capture differently and are not
  affected.

## Running

The tests assume they're invoked from a checkout layout like Antmicro's:

```
renode-xous-precursor/
├── xous.resc                          # machine setup script
├── xous-core/                         # sibling checkout
│   └── tools/pddb-images/renode.bin   # flash backing file
└── tests/renode/                      # this directory (or symlinked in)
```

Then:

```
renode-test tests/renode/pddb-format.robot
```

## Files

- `pddb-format.robot` — drives the PDDB format ceremony from a blank flash
  and captures the resulting flash image for reuse. Two test cases:
  *Format PDDB And Save Flash* (~14 min) and *Reuse Saved Flash* (~1 min).
  Adapted from antmicro/renode-xous-precursor.

## Project-specific finding

The PDDB format ceremony exposed a Xous GAM behaviour worth recording: the
radiobutton modal does **not** submit on Enter alone. The modal has items
at indices `0..items.len()-1` plus an explicit "OK button" row at index
`items.len()`. Enter at an item row only selects that item; Enter at the
OK button row submits and closes. Cursor defaults to index 0.

For dialogs with N items (typically 2 for Okay/Cancel), the submit
sequence is `Arrow Down × N + Enter`. Without this navigation, the dialog
appears to hang and llio time-offset spam dominates the UART until the
test times out. The `Format PDDB And Save Flash` test documents and
applies this for the format-confirmation dialog (Arrow Down × 2 + Enter).
