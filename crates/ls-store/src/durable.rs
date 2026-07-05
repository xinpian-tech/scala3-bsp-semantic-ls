//! Shared durability helpers for the manifest / workspace-state commit points:
//! write a temp file, fsync it, atomically rename it into place, and fsync the
//! parent directory. The same fsync + atomic-rename protocol the segment writer
//! uses, factored out for the single-file commit points.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::error::StoreResult;

/// fsync a directory so a rename within it is durable.
pub(crate) fn fsync_dir(dir: &Path) -> StoreResult<()> {
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Write `bytes` to `dir/tmp_name`, fsync it, then atomically rename it to
/// `dir/final_name` and fsync `dir`.
pub(crate) fn atomic_write(
    dir: &Path,
    tmp_name: &str,
    final_name: &str,
    bytes: &[u8],
) -> StoreResult<()> {
    write_tmp(dir, tmp_name, bytes)?;
    fs::rename(dir.join(tmp_name), dir.join(final_name))?;
    fsync_dir(dir)?;
    Ok(())
}

/// Write `bytes` to `dir/tmp_name` and fsync the file (but do not rename). Used
/// directly by the publish protocol so a failpoint can be injected between the
/// tmp write and the rename.
pub(crate) fn write_tmp(dir: &Path, tmp_name: &str, bytes: &[u8]) -> StoreResult<()> {
    let mut f = fs::File::create(dir.join(tmp_name))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
