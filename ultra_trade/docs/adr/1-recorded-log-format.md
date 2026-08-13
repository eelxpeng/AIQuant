# Recorded event log format

- **Issue**: #1 (needs its own issue — see "Naming")
- **Status**: proposed
- **Date**: 2026-08-12

## Naming

`docs/adr/README.md` names ADRs after their owning issue. This one has none
yet: the format is forced by D0.2 and by `adapters/historical` (Phase 1), and
neither has an issue. It is filed under the roadmap hub, like ADR #1, and
should be renamed once an issue exists. Agents do not open workstream issues.

Unlike ADR #1, this record is written **before** the code it decides, which is
what `docs/adr/README.md` asks for.

## Context

`MemoryLog` persists nothing. That blocks three things at once:

- **D1.2** — "book building reproduces a recorded session's top-of-book exactly
  on replay" needs a recorded session to exist.
- **`adapters/historical`** — the crate table's recorded-market-data feed has
  nothing to read.
- **A real backtest** — `bin/backtest` runs on a synthetic triangular wave, so
  it proves the wiring and not a strategy.

Getting this wrong is expensive in a specific way: a format is the one artifact
that outlives every binary that wrote it. A session recorded today has to be
readable after the value representation changes (ADR #4 already anticipates
that), after a variant is added to the event alphabet, and after a crash
truncates the file mid-write.

What breaks if we get it wrong: a backtest silently reads instrument 3 as a
different contract than the one recorded; a torn tail is read as a short but
complete session; or a float creeps into a durable record and two readers
disagree about a price.

## Decision

### D-1 · One durable format: the event log

There is one persisted format, and it is the event log. `adapters/historical`
replays the `Inbound` records of a recorded log, so **a backtest input is just a
past session**. One format, one versioning story, one set of fixtures.

Raw venue wire captures are **out of scope**. They are inherently
venue-specific — there is no single raw format to standardise — so they belong
to whichever adapter wants them.

*Accepted risk, stated plainly*: a normalization bug is permanent. Because the
raw bytes are not kept, historical events can never be re-derived differently.
If a feed adapter mis-normalizes, every session recorded through it is wrong and
stays wrong. Keeping raw captures per adapter is the mitigation, and it is a
separate decision.

### D-2 · Fixed-size 80-byte records

Record *n* lives at `header_len + n * 80`. Two properties follow, and both are
the reason for the choice:

- **Seeking to a sequence number is arithmetic**, not an index lookup. That
  matches the in-memory design, where `Seq` is already a dense position rather
  than an opaque key.
- **A torn tail is detectable from the file length alone**:
  `(len - header_len) % 80 != 0`.

The layout:

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `seq`, u64 |
| 8 | 2 | `version`, u16 — per record, so a file may span a binary upgrade |
| 10 | 1 | `kind`, u8 — explicit discriminant, see D-4 |
| 11 | 1 | `flags`, u8 — currently only reduce-only |
| 12 | 56 | payload, kind-specific, zero-padded |
| 68 | 8 | reserved, **must be zero** |
| 76 | 4 | `crc32`, u32 over bytes 0..76 |

**80 rather than 64.** 64 was the intent and it does not fit: a fill's fee is a
`Notional`, which is `i128`, so `Venue/Filled` needs 56 payload bytes against 16
bytes of framing — 72 minimum. Rounding to 80 leaves 8 reserved bytes so that
one more `i64` field can be added later without immediately spending a version.
Reserved bytes must be zero and are checked on read, so a writer that scribbles
there is caught rather than tolerated.

Cache-line alignment is not a consideration: records are encoded field by field
into a buffer, never transmuted, because the crate forbids `unsafe`.

Cost: roughly 35% padding on a quote-heavy session. Accepted for O(1) seek and
trivial torn-tail detection.

### D-3 · The header carries the instrument table

A log that says `InstrumentId(3)` and nothing else is not interpretable. The
header records the configuration the session ran under:

- magic `ULTRALOG`, format version, header length, record length
- session id (caller-supplied `u64` — never from a clock or a random source)
- session start on the exchange clock, and the first order id
- the **instrument table**: for each id, its symbol, tick, lot, and minimum
  quantity

`Instrument` in `types` carries no symbol, and this does not add one — the
symbol is a property of the *session's configuration*, recorded here, not of the
in-memory value.

**A reader must reject a file whose instrument table disagrees with the session
it is being replayed into.** Without that check, `InstrumentId(3)` silently
means a different contract and the backtest is wrong in a way no test would
catch.

### D-4 · Discriminants are explicit and never reused

The `kind` byte is assigned by hand, not derived from a Rust enum's declaration
order. Reordering a variant must not change what a recorded byte means. A
retired kind's number is never given to something else; readers of a newer
format encountering an unknown kind fail rather than guess.

### D-5 · Little-endian, integers only

Every field is written little-endian via `to_le_bytes`. No native-endian
encoding, no `unsafe` reinterpretation, and **no floating point anywhere** —
every value is already the scaled integer the fixed-point types hold
(Constitution VII).

### D-6 · CRC32 per record, hand-rolled

Each record carries a CRC32 over its own bytes. Per record rather than per file,
because a file-level checksum needs a footer, and a crash is exactly the case
where the footer never gets written.

The implementation is a small table-driven CRC32 in `event`, not a dependency.
`types` has zero dependencies and the workspace has two dev-dependencies in
total; a forty-line function with a known test vector is cheaper than another
crate in a real-money system's supply chain.

### D-7 · Recovery truncates to the last intact record, and says so

A reader stops at the first record that fails its CRC, is misaligned, or has a
non-zero reserved field, and **reports what it dropped**. It never silently
returns a short log, because a short log is indistinguishable from a complete
one to everything downstream.

### D-8 · Bounded tail loss, written off the hot path

The engine hands records to a writer that persists them away from the hot path.
It cannot do otherwise: the hot path admits no syscall (Constitution VI), and a
write is a syscall.

The consequence is that a crash loses a bounded tail, and recovery truncates to
the last intact record. **What a lost tail means for position is not settled
here** — that is the crash-recovery contract, D4.4, and it is the reason
per-record CRC and torn-tail detection are in this format rather than being left
to the writer.

## Alternatives rejected

| Alternative | Why it lost |
|---|---|
| Variable-length, length-prefixed records | ~35% smaller, but seeking to a `Seq` needs a side index file, and a torn tail can only be found by walking. The index becomes a second thing that can disagree with the log. |
| JSON or CSV | Debuggable, and every other property is wrong: far larger, slow to parse, and it invites a float into a durable money field. A text *export* tool is a fine separate thing. |
| CBOR / MessagePack / `serde` | Self-describing and flexible, at the cost of a dependency tree and variable record sizes — losing the O(1) seek that motivated D-2. Flexibility is not the goal; a format that changes shape without a version bump is the failure mode. |
| Native-endian struct dumps | Fastest to write, requires `unsafe`, and produces files that are not portable between machines. The crate forbids `unsafe`. |
| File-level checksum in a footer | The footer is missing in exactly the case it is needed: a crash mid-session. |
| Deriving `kind` from the enum's discriminant | Reordering a variant would silently change what every recorded byte means. |
| `fsync` per event | Nothing is ever lost, at the cost of a syscall and a disk flush on the engine thread per event. Constitution VI forbids the syscall, and the latency budget would not survive the flush. |

## Consequences

**What this constrains.** The event alphabet is now a durable interface. Adding
a variant means assigning a new discriminant; changing a field's width means a
version bump and a migration path. A payload that outgrows 56 bytes forces a
record-size change, which is a new format version — the 8 reserved bytes buy one
`i64` of headroom before that happens.

**What it costs.** About 35% storage overhead against a packed encoding, and a
hand-maintained encoder and decoder per record kind. The decoder is the part
that must be tested hardest: a round-trip property test over every kind, plus
explicit cases for a torn tail, a bad CRC, a non-zero reserved field, an unknown
kind, and an instrument table that disagrees with the session.

**What it does not settle.** The crash-recovery contract (D4.4) — what a
truncated tail means for position and whether a session may resume from one. And
the writer itself: this record fixes the bytes, not the threading.

**What would make us revisit.** A record kind that cannot fit 56 bytes; a
storage bill where 35% matters; or a decision to keep raw venue captures, which
would make D-1's accepted risk unnecessary.
