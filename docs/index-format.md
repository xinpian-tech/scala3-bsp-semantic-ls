# mmap postings segment format — v1

Normative specification of the on-disk index segment format implemented by
`ls.postings` (`modules/ls-postings`). Plan sections 8, 9.3 and 17 are the
design rationale; where this document and the plan differ in detail, this
document describes what is actually on disk.

* **Version**: 1 (`header.bin` `version` field).
* **Byte order**: every multi-byte scalar in every file is **little-endian**.
* **Alignment**: no padding between records beyond what is specified below.
  Readers must use unaligned loads (`ValueLayout.*_UNALIGNED`).
* **Strings**: UTF-8, referenced by `(offset, length)` into a per-file blob.
  No NUL terminators.
* **Checksums**: CRC32C (Castagnoli), stored as a `uint32` value
  zero-extended into an `int64` field.

## v1 simplification: one segment per generation

In v1 **one segment is one complete index generation**: every publish writes
a full new segment and a snapshot reads exactly one active segment. There is
no multi-segment layering yet.

The format nevertheless keeps the invariants that layering will need
(plan 9.3):

* every group-postings record carries its own `doc_epoch`;
* readers must drop any record whose `doc_epoch` differs from the epoch of
  its `doc_ord` in the doc dictionary, even though in a v1 segment the two
  always match by construction;
* block-index entries carry `epoch_min`/`epoch_max` so future readers can
  skip whole stale blocks.

Adding layered segments later is therefore a reader/manifest change, not an
on-disk format change.

## Directory layout and publication protocol

```
<postings root>/
  tmp-<segment_id>/              # writer scratch; crash debris only
  segments/
    segment-NNNNNN/              # zero-padded decimal segment_id (%06d)
      header.bin
      ref-group-index.bin
      definition-group-index.bin
      rename-group-index.bin
      doc-index.bin
      symbol-index.bin
      ref-postings.bin
      definition-postings.bin
      rename-postings.bin
      doc-postings.bin
      block-index.bin
      checksums.bin
```

Writer protocol (`ls.postings.SegmentWriter`):

1. build all twelve files in memory;
2. write them into `<root>/tmp-<segment_id>`, `fsync` every file;
3. `fsync` the tmp directory;
4. atomically rename `tmp-<segment_id>` →
   `segments/segment-NNNNNN` (`ATOMIC_MOVE`);
5. `fsync` the `segments/` directory.

A segment directory that exists under `segments/` is therefore always
complete. Readers must reject (never partially serve) any segment that fails
validation (magic, version, header checksum, any file CRC, structural size
checks).

## Identifier conventions

Persistent ids (`doc_id`, `symbol_id`, `target_id`, SQLite side) are `int64`.
Snapshot ordinals (`doc_ord`, `target_ord`, `symbol_ord`, `ref_group_ord`,
`rename_group_ord`) are dense `int32` indices valid only inside this segment;
they index directly into the dense arrays below — lookups are O(1), no binary
search (plan 8.4). `-1` encodes "none/unknown" wherever a field is optional.

`symbol_ord` is the index of a symbol in `symbol-index.bin`, which is sorted
by the UTF-8 bytes of the semantic symbol string (unsigned byte-wise
comparison), so symbol lookup by string is a binary search over fixed-width
entries.

Positions are packed as in `ls.index.Span.pack`:

```
packed = (line << 12) | character      // line saturates at 2^20-1, char at 2^12-1
```

Occurrence `flags` use `ls.index.OccFlags` bits:
`1<<0` Definition, `1<<1` Editable, `1<<2` Generated, `1<<3` Readonly,
`1<<4` Synthetic.

## header.bin (64 bytes)

| offset | type   | field              |
|-------:|--------|--------------------|
| 0      | uint32 | magic = `0x4750534C` (bytes `L S P G` in file order) |
| 4      | uint16 | version = 1        |
| 6      | uint16 | flags = 0          |
| 8      | uint64 | segment_id         |
| 16     | uint64 | created_at_ms      |
| 24     | uint64 | ref_group_count    |
| 32     | uint64 | rename_group_count |
| 40     | uint64 | doc_count          |
| 48     | uint64 | occurrence_count   |
| 56     | uint64 | checksum = CRC32C of bytes `[0, 56)` |

`occurrence_count` is the total record count of `ref-postings.bin` +
`definition-postings.bin` + `rename-postings.bin` + `doc-postings.bin`.

## Group index files

`ref-group-index.bin` and `definition-group-index.bin` both have
`ref_group_count` entries (references and definitions share the
`ref_group_ord` space); `rename-group-index.bin` has `rename_group_count`
entries.

```
int64 group_count
GroupIndexEntry[group_count]        // 16 bytes each
```

`GroupIndexEntry`:

| offset | type  | field |
|-------:|-------|-------|
| 0      | int64 | offset — ordinal of the group's first record in the corresponding postings file |
| 8      | int32 | count — number of records in the group |
| 12     | int32 | block_index_offset — ordinal of the group's first entry in `block-index.bin`, `-1` iff `count == 0` |

A group's records are contiguous: records `[offset, offset+count)`, blocks
`[block_index_offset, block_index_offset + ceil(count/256))`.

`rename-group-index.bin` additionally appends the rename profiles
(plan 13.2), one per group, directly after the entry array:

```
RenameProfileEntry[group_count]     // 16 bytes each
```

`RenameProfileEntry`:

| offset | type  | field |
|-------:|-------|-------|
| 0      | int32 | profile_flags: bit0 is_local, bit1 is_external, bit2 has_generated_occurrences, bit3 has_readonly_occurrences, bit4 has_override_family, bit5 has_companion |
| 4      | int32 | editable_occurrence_count |
| 8      | int64 | unsafe_reason_mask (`ls.index.UnsafeReason` bits) |

These are the 8 `ls.index.RenameProfile` fields.

## Group postings files (columnar)

`ref-postings.bin`, `definition-postings.bin`, `rename-postings.bin` share
one layout (plan 8.5):

```
int64 record_count
int32 doc_ord      [record_count]
int32 doc_epoch    [record_count]
int32 target_ord   [record_count]
int32 packed_start [record_count]
int32 packed_end   [record_count]
int32 flags        [record_count]
```

Column base offsets are `8 + column_index * 4 * record_count`.

Sort order: within each group, records are sorted by
`(doc_ord, packed_start, packed_end)` (ties keep writer input order — the
sort is stable).

Role separation (plan 8.6): `ref-postings.bin` holds reference-role
occurrences per `ref_group_ord`; `definition-postings.bin` holds
definition-role occurrences per `ref_group_ord` (their `flags` carry the
Definition bit); `rename-postings.bin` holds the rename edit candidates per
`rename_group_ord`.

**Reader obligations for group scans** (in this order):

1. block skip: skip a block when its target bitset does not intersect the
   allowed target set; for rename scans also skip when
   `editable_count == 0`;
2. per-record target filter against the allowed target set (when one is
   given);
3. epoch filter: drop the record unless
   `doc_epoch == DocEntry[doc_ord].epoch`;
4. rename scans additionally surface only records whose `flags` have the
   Editable bit (the block `editable_count` skip is an optimization, the
   per-record test is the exact rule).

## doc-index.bin (doc dictionary + interval block index)

```
int64 doc_count
int64 interval_entry_count
int64 uri_blob_length
DocEntry[doc_count]                  // 48 bytes each
IntervalEntry[interval_entry_count]  // 24 bytes each
byte  uri_blob[uri_blob_length]
```

`DocEntry` (indexed by `doc_ord`):

| offset | type  | field |
|-------:|-------|-------|
| 0      | int32 | uri_offset (into uri_blob) |
| 4      | int32 | uri_len |
| 8      | int64 | doc_id (persistent) |
| 16     | int32 | epoch — current epoch of this document |
| 20     | int32 | target_ord |
| 24     | int32 | doc_flags: bit0 generated, bit1 readonly |
| 28     | int32 | interval_first — ordinal of first IntervalEntry, `-1` iff no postings |
| 32     | int64 | postings_offset — first record ordinal in `doc-postings.bin` |
| 40     | int32 | postings_count |
| 44     | int32 | interval_count |

`IntervalEntry` (plan 8.7; per-doc runs, each covering ≤ 256 records of
`doc-postings.bin`, never spanning two docs):

| offset | type  | field |
|-------:|-------|-------|
| 0      | int32 | first_line — start line of the block's first record |
| 4      | int32 | last_line — max end line over the block's records |
| 8      | int64 | offset — first record ordinal in `doc-postings.bin` |
| 16     | int32 | count |
| 20     | int32 | pad = 0 |

Within one doc, `first_line` is non-decreasing across blocks (records are
sorted by `packed_start`), so a position lookup may stop at the first block
with `first_line > line`; `last_line` is not monotonic (multi-line
occurrences), so every earlier block with `last_line >= line` must be
scanned.

## doc-postings.bin (columnar)

```
int64 record_count
int32 symbol_ord   [record_count]
int32 packed_start [record_count]
int32 packed_end   [record_count]
int32 flags        [record_count]    // role bit = OccFlags.Definition
```

Within each doc, records are sorted by `(packed_start, packed_end)` (stable).
Doc postings carry no per-record `target_ord`/`doc_epoch`: a v1 segment
stores exactly one (current-epoch) postings run per document, and scans
report the doc dictionary's `target_ord` and `epoch`.

**Editable doc view**: `IndexSnapshot.scanDocEditable(doc, sink)` is a filtered
scan over this same file, not a separate materialization — records already carry
the `OccFlags.Editable` bit, so the reader emits only records with that bit set
(the editable subset of `scanDocOccurrences`). A dedicated `doc -> editable`
postings file is deliberately not written.

**symbol-at-position rule**: containment is start-inclusive and
end-inclusive on packed positions
(`packed_start <= packed(query) <= packed_end`, matching
`ls.index.Span.contains`); among covering occurrences the smallest
(`packed_end - packed_start`) wins; among equally small ones the earliest
`packed_start` wins; among identical spans the first record in sort order
wins.

## symbol-index.bin (symbol + target dictionaries)

```
int64 symbol_count
int64 target_count
int64 sym_blob_length
SymbolEntry[symbol_count]            // 32 bytes each, sorted by UTF-8 bytes
int64 target_id[target_count]        // persistent target id per target_ord
byte  sym_blob[sym_blob_length]
```

`SymbolEntry` (indexed by `symbol_ord`):

| offset | type  | field |
|-------:|-------|-------|
| 0      | int32 | str_offset (into sym_blob) |
| 4      | int32 | str_len |
| 8      | int64 | symbol_id (persistent) |
| 16     | int32 | ref_group_ord (`-1` = none) |
| 20     | int32 | rename_group_ord (`-1` = none) |
| 24     | int32 | def_target_ord (`-1` = unknown) |
| 28     | int32 | pad = 0 |

Entries are sorted ascending by unsigned byte-wise comparison of the UTF-8
semantic symbol string; duplicate symbol strings are invalid.

## block-index.bin (exact skip metadata, plan 8.8)

One shared file for the three group-postings files. Blocks are appended in
writer order: all ref groups, then all definition groups, then all rename
groups; `GroupIndexEntry.block_index_offset` points into this sequence.
Every block covers ≤ 256 records (`block_size`) of its owning postings file
and never spans two groups.

```
int64 block_count
int32 target_word_count    // W = max(1, ceil(target_count / 64))
int32 block_size = 256
BlockEntry[block_count]    // 40 + 8*W bytes each
```

`BlockEntry`:

| offset | type  | field |
|-------:|-------|-------|
| 0      | int64 | first_record — record ordinal in the owning postings file |
| 8      | int32 | record_count (≤ 256) |
| 12     | int32 | editable_count — records with the Editable flag |
| 16     | int32 | ref_role_count — records without the Definition flag |
| 20     | int32 | def_role_count — records with the Definition flag |
| 24     | int32 | doc_ord_min |
| 28     | int32 | doc_ord_max |
| 32     | int32 | epoch_min |
| 36     | int32 | epoch_max |
| 40     | int64 | target_words[W] — exact bitset over `target_ord` (bit `t` = word `t>>6`, bit `t&63`) |

All metadata is exact (real sets and counts, never probabilistic — no Bloom
filters, plan 8.8).

## checksums.bin

```
int64 entry_count = 11
repeat entry_count times:
  int32 name_len
  byte  name[name_len]      // UTF-8 file name
  int64 crc32c              // CRC32C of that file's entire contents
```

Entries appear in the canonical order: `header.bin`,
`ref-group-index.bin`, `definition-group-index.bin`,
`rename-group-index.bin`, `doc-index.bin`, `symbol-index.bin`,
`ref-postings.bin`, `definition-postings.bin`, `rename-postings.bin`,
`doc-postings.bin`, `block-index.bin`. `checksums.bin` itself is not
checksummed; `header.bin` is both self-checksummed (its trailing field) and
listed here (whole-file CRC).

A reader must verify every entry at open and reject the segment on the first
mismatch, missing file, unexpected entry name or count.

## Open-time validation (implemented by `ls.postings.SegmentReader.open`)

1. `header.bin` is exactly 64 bytes, magic and version match, header
   checksum matches;
2. `checksums.bin` lists exactly the 11 files above, in order, and every CRC
   matches the mapped file bytes;
3. structural cross-checks: group-index counts match header counts and file
   sizes; postings file sizes match `8 + columns*4*record_count`; the sum of
   the four postings record counts equals `occurrence_count`; doc-index and
   symbol-index sizes match their declared counts and blob lengths;
   block-index `block_size` is 256 and `target_word_count` matches
   `target_count`.

Any failure raises `SegmentCorruptedException`; the mapping arena is closed
and the segment is never served.

## Snapshot lifecycle (plan 9.1, implemented by `ls.postings`)

* `PostingsSnapshot` implements `ls.index.IndexSnapshot` over one mapped
  segment; all files live in one shared FFM `Arena`.
* Reference counting: the count starts at 1 (creator reference held by
  `SnapshotManager`); `retain()` refuses once the snapshot has been
  superseded (close initiated) or drained; `release()` closes the arena when
  the count drains to 0. A reader that retained before a publish keeps a
  fully usable snapshot until it releases.
* `SnapshotManager.publish` swaps an `AtomicReference`, marks the previous
  snapshot superseded, drops the creator reference and queues the segment
  directory for `deleteSuperseded()` — the v1 compactor job, which deletes
  a superseded directory only after its arena has fully closed.
