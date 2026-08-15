# docs/

| File | What it is |
|---|---|
| [`TESTS.md`](TESTS.md) | The conformance baseline against the hardware suite |
| [`MEMORY_MAP.md`](MEMORY_MAP.md) | The address space, and what is decoded vs stubbed |
| [`SAVESTATE.md`](SAVESTATE.md) | The state layout and the rules it is bound by |
| [`notes/`](notes/) | Per-subsystem implementation notes, decisions and open questions |
| [`ref/`](ref/) | Derived hardware reference: what the machine does, with citations |

`ref/` describes the **hardware**; `notes/` describes **this codebase's** take on
it, including where the two currently disagree. Only markdown is tracked under
`docs/`: raw third-party drops (PDFs, HTML, archives) stay out of git.

## The clean-room rule

Subsystems are written from hardware documentation, never from another
emulator's source. The working method is:

1. Read the reference material in `docs/ref/`.
2. Distil what the hardware does into a note in `docs/notes/`, in your own
   words, with the behaviour stated as behaviour rather than as code.
3. Implement from the note.

Step 2 is not paperwork. It is what makes the implementation defensible, and it
is where the open questions get written down instead of being resolved by
guessing in the middle of a function.

Measured reference *data* (a hardware colour table, a timing measurement) may be
used as data, and is cited where it is used.
