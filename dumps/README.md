# dumps/

Test binaries, disc images, save states and harness output. **Nothing in this
folder is committed** except this file. The ignore rule is a blanket one on the
whole tree, so no copyrighted image can be committed by accident regardless of
which subfolder it lands in.

## Suggested shape

```
dumps/cpu/       R3000A conformance binaries (PSX-EXE)
dumps/gte/       GTE conformance binaries (PSX-EXE)
dumps/gpu/       GPU / timing demos
dumps/discs/     commercial images, once there is anything to run them with
dumps/states/    save states captured from a session
```

The layout is a suggestion, not a contract. `testrom --dir` walks whatever it is
given, recursively, and sorts the results so a run is reproducible.

## What the harness expects

A **PSX-EXE**: the 2048-byte `PS-X EXE` header followed by the text. That is what
the CPU and GTE suites ship as, and it is the only content format the core reads
today.

The harness boots the real BIOS first and swaps the binary in at the shell
hand-over point (PC `0x80030000`), so the test runs with the kernel, the A/B/C
function tables and the TTY already up. Its printed output is captured through
the BIOS TTY call gates and is what the run is graded on.

A suite that prints **nothing** is graded as a failure, not a pass: silence is
exactly what a core that never reached the test looks like.
