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

/** Recomputes the boundary layout canary from this side's jextract
  * `sizeof()`/`$offset()` accessors — the cross-language contract with
  * `compute_layout_canary` in the Rust `ls-pc-abi` crate.
  *
  * At bootstrap the premain compares this value against the canary the Rust
  * side embedded in its vtable; a mismatch means the two sides disagree on the
  * binary layout (a same-size field reorder or a wrong slot offset), so
  * registration is refused. The facts cover the size AND every field offset of
  * every boundary struct, plus every `RustVtable`/`PcVtable` slot offset, in
  * exactly the order the Rust `facts()` list uses.
  */
object LayoutCanary:
  // FNV-1a (64-bit). Must stay byte-for-byte identical to the Rust side: the
  // same 51 layout facts, in the same order, each hashed as 8 little-endian
  // bytes.
  private val FnvOffset: Long = 0xcbf29ce484222325L
  private val FnvPrime: Long = 0x100000001b3L

  /** The ordered layout facts: for every boundary struct its size followed by
    * every field offset, then each vtable's size followed by every slot offset.
    * The order mirrors the Rust `facts()` array exactly.
    */
  def facts(): Array[Long] =
    Array[Long](
      // LsStr.
      LsStr.sizeof(),
      LsStr.`ptr$offset`(),
      LsStr.`len$offset`(),
      // LsBytes.
      LsBytes.sizeof(),
      LsBytes.`ptr$offset`(),
      LsBytes.`len$offset`(),
      // LsBuf.
      LsBuf.sizeof(),
      LsBuf.`ptr$offset`(),
      LsBuf.`len$offset`(),
      // BlobStr.
      BlobStr.sizeof(),
      BlobStr.`offset$offset`(),
      BlobStr.`len$offset`(),
      // Position.
      Position.sizeof(),
      Position.`line$offset`(),
      Position.`character$offset`(),
      // AbiRange.
      AbiRange.sizeof(),
      AbiRange.`start_line$offset`(),
      AbiRange.`start_character$offset`(),
      AbiRange.`end_line$offset`(),
      AbiRange.`end_character$offset`(),
      // LocationRecord.
      LocationRecord.sizeof(),
      LocationRecord.`uri$offset`(),
      LocationRecord.`range$offset`(),
      LocationRecord.`origin$offset`(),
      // Rust vtable: size + every slot offset.
      RustVtable.sizeof(),
      RustVtable.`abi_version$offset`(),
      RustVtable.`layout_canary$offset`(),
      RustVtable.`alloc$offset`(),
      RustVtable.`free$offset`(),
      RustVtable.`log$offset`(),
      RustVtable.`register_pc_vtable$offset`(),
      RustVtable.`pc_dispatch_loop$offset`(),
      RustVtable.`symbol_definition$offset`(),
      RustVtable.`search_methods$offset`(),
      // PC vtable: size + every slot offset.
      PcVtable.sizeof(),
      PcVtable.`abi_version$offset`(),
      PcVtable.`register_target$offset`(),
      PcVtable.`did_open$offset`(),
      PcVtable.`did_change$offset`(),
      PcVtable.`did_close$offset`(),
      PcVtable.`completion$offset`(),
      PcVtable.`completion_resolve$offset`(),
      PcVtable.`hover$offset`(),
      PcVtable.`signature_help$offset`(),
      PcVtable.`definition$offset`(),
      PcVtable.`type_definition$offset`(),
      PcVtable.`prepare_rename$offset`(),
      PcVtable.`plugin_status$offset`(),
      PcVtable.`restart_instances$offset`(),
      PcVtable.`shutdown$offset`(),
      PcVtable.`spawn_dispatch$offset`()
    )

  /** Computes the layout canary from the ordered facts. */
  def compute(): Long =
    val values = facts()
    var hash = FnvOffset
    var i = 0
    while i < values.length do
      val value = values(i)
      var b = 0
      while b < 8 do
        hash ^= (value >>> (b * 8)) & 0xffL
        hash *= FnvPrime
        b += 1
      i += 1
    hash
