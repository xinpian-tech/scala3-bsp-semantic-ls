package ls.postings

import java.nio.file.{Files, Path}

import TestSupport.*

/** Segment open must reject any file whose CRC32C no longer matches
  * checksums.bin, and headers with bad magic/version/checksum.
  */
class CorruptionTest extends munit.FunSuite:

  private def freshSegment(name: String): Path =
    SegmentWriter.write(tempRoot(name), 1, TestSupport.randomCorpus(0xdead_beefL))

  private def flipByte(file: Path, offset: Int): Unit =
    val bytes = Files.readAllBytes(file)
    bytes(offset) = (bytes(offset) ^ 0x5a).toByte
    Files.write(file, bytes)

  test("a clean segment opens"):
    val dir = freshSegment("clean")
    val reader = SegmentReader.open(dir)
    try assertEquals(reader.segmentId, 1L)
    finally reader.close()

  test("one flipped byte in ref-postings.bin is rejected at open"):
    val dir = freshSegment("flip-post")
    val file = dir.resolve(SegmentFormat.RefPostingsFile)
    flipByte(file, (Files.size(file) / 2).toInt)
    val e = intercept[SegmentCorruptedException](SegmentReader.open(dir))
    assert(e.getMessage.contains(SegmentFormat.RefPostingsFile), e.getMessage)

  test("a flipped byte in doc-index.bin is rejected at open"):
    val dir = freshSegment("flip-docidx")
    val file = dir.resolve(SegmentFormat.DocIndexFile)
    flipByte(file, (Files.size(file) / 3).toInt)
    intercept[SegmentCorruptedException](SegmentReader.open(dir))

  test("bad magic is rejected"):
    val dir = freshSegment("flip-magic")
    flipByte(dir.resolve(SegmentFormat.HeaderFile), 0)
    val e = intercept[SegmentCorruptedException](SegmentReader.open(dir))
    assert(e.getMessage.contains("magic"), e.getMessage)

  test("bad version is rejected"):
    val dir = freshSegment("flip-version")
    flipByte(dir.resolve(SegmentFormat.HeaderFile), 4)
    val e = intercept[SegmentCorruptedException](SegmentReader.open(dir))
    assert(e.getMessage.contains("version"), e.getMessage)

  test("corrupted header body fails its own checksum"):
    val dir = freshSegment("flip-header")
    flipByte(dir.resolve(SegmentFormat.HeaderFile), 24) // ref_group_count
    val e = intercept[SegmentCorruptedException](SegmentReader.open(dir))
    assert(e.getMessage.contains("checksum"), e.getMessage)

  test("missing file is rejected"):
    val dir = freshSegment("missing")
    Files.delete(dir.resolve(SegmentFormat.BlockIndexFile))
    intercept[SegmentCorruptedException](SegmentReader.open(dir))
