package ls.pc.host

import ls.pc.host.boundary.{
  AbiRange,
  BlobStr,
  LocationRecord,
  LsBuf,
  LsBytes,
  LsStr,
  PcVtable,
  Position,
  RustVtable
}

/** Two-sided layout verification (the island half of the boot canary).
  *
  * These offset unit tests pin the jextract-mirrored boundary layout to the
  * exact `#[repr(C)]` contract the Rust `ls-pc-abi` crate defines, and assert
  * that the independently recomputed layout canary equals the constant the Rust
  * side embeds (`ls_pc_abi::LAYOUT_CANARY`). A same-size field reorder or a
  * wrong slot offset on either side breaks these before it can reach a running
  * boundary.
  */
class LayoutSuite extends munit.FunSuite:

  // The value of `ls_pc_abi::LAYOUT_CANARY` (FNV-1a over the 51 ordered layout
  // facts). If the Rust ABI changes, regenerate the bindings and update this.
  private val RustLayoutCanary: Long = 0x9e1bfa41f279e689L

  test("string/buffer argument structs are two pointer-sized words"):
    assertEquals(LsStr.sizeof(), 16L)
    assertEquals(LsStr.`ptr$offset`(), 0L)
    assertEquals(LsStr.`len$offset`(), 8L)
    assertEquals(LsBytes.sizeof(), 16L)
    assertEquals(LsBytes.`ptr$offset`(), 0L)
    assertEquals(LsBytes.`len$offset`(), 8L)
    assertEquals(LsBuf.sizeof(), 16L)
    assertEquals(LsBuf.`ptr$offset`(), 0L)
    assertEquals(LsBuf.`len$offset`(), 8L)

  test("blob string and position are two u32 words"):
    assertEquals(BlobStr.sizeof(), 8L)
    assertEquals(BlobStr.`offset$offset`(), 0L)
    assertEquals(BlobStr.`len$offset`(), 4L)
    assertEquals(Position.sizeof(), 8L)
    assertEquals(Position.`line$offset`(), 0L)
    assertEquals(Position.`character$offset`(), 4L)

  test("flattened range is four u32 words"):
    assertEquals(AbiRange.sizeof(), 16L)
    assertEquals(AbiRange.`start_line$offset`(), 0L)
    assertEquals(AbiRange.`start_character$offset`(), 4L)
    assertEquals(AbiRange.`end_line$offset`(), 8L)
    assertEquals(AbiRange.`end_character$offset`(), 12L)

  test("location record packs blob-uri, range, and origin ordinal"):
    assertEquals(LocationRecord.`uri$offset`(), 0L)
    assertEquals(LocationRecord.`range$offset`(), 8L)
    assertEquals(LocationRecord.`origin$offset`(), 24L)
    assertEquals(LocationRecord.sizeof(), 28L)

  test("rust vtable is nine 8-byte slots in the contract order"):
    assertEquals(RustVtable.sizeof(), 72L)
    assertEquals(RustVtable.`abi_version$offset`(), 0L)
    assertEquals(RustVtable.`layout_canary$offset`(), 8L)
    assertEquals(RustVtable.`alloc$offset`(), 16L)
    assertEquals(RustVtable.`free$offset`(), 24L)
    assertEquals(RustVtable.`log$offset`(), 32L)
    assertEquals(RustVtable.`register_pc_vtable$offset`(), 40L)
    assertEquals(RustVtable.`pc_dispatch_loop$offset`(), 48L)
    assertEquals(RustVtable.`symbol_definition$offset`(), 56L)
    assertEquals(RustVtable.`search_methods$offset`(), 64L)

  test("pc vtable is abi_version then 15 slots in the contract order"):
    assertEquals(PcVtable.sizeof(), 128L)
    assertEquals(PcVtable.`abi_version$offset`(), 0L)
    assertEquals(PcVtable.`register_target$offset`(), 8L)
    assertEquals(PcVtable.`did_open$offset`(), 16L)
    assertEquals(PcVtable.`did_change$offset`(), 24L)
    assertEquals(PcVtable.`did_close$offset`(), 32L)
    assertEquals(PcVtable.`completion$offset`(), 40L)
    assertEquals(PcVtable.`completion_resolve$offset`(), 48L)
    assertEquals(PcVtable.`hover$offset`(), 56L)
    assertEquals(PcVtable.`signature_help$offset`(), 64L)
    assertEquals(PcVtable.`definition$offset`(), 72L)
    assertEquals(PcVtable.`type_definition$offset`(), 80L)
    assertEquals(PcVtable.`prepare_rename$offset`(), 88L)
    assertEquals(PcVtable.`plugin_status$offset`(), 96L)
    assertEquals(PcVtable.`restart_instances$offset`(), 104L)
    assertEquals(PcVtable.`shutdown$offset`(), 112L)
    assertEquals(PcVtable.`spawn_dispatch$offset`(), 120L)

  test("the fact list is exactly the 51 the Rust side hashes"):
    assertEquals(LayoutCanary.facts().length, 51)

  test("recomputed canary equals the Rust LAYOUT_CANARY"):
    assertEquals(LayoutCanary.compute(), RustLayoutCanary)

  test("the canary is deterministic and non-trivial"):
    assertEquals(LayoutCanary.compute(), LayoutCanary.compute())
    assertNotEquals(LayoutCanary.compute(), 0L)
    assertNotEquals(LayoutCanary.compute(), 0xcbf29ce484222325L)
