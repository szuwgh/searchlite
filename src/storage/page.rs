use crate::storage::fs::create_and_ensure_length;
use crate::storage::fs::open_write_mmap;
use crate::util::error::GyResult;
use memmap2::MmapMut;
use std::path::Path;
use std::path::PathBuf;
//一个文件一页 32MB
pub(super) struct Page {
    pub(super) path: PathBuf,
    pub(super) mmap: MmapMut,
}

impl Page {
    pub(crate) fn flush(&self) -> std::io::Result<()> {
        self.mmap.flush()
    }

    fn new(path: &Path, size: usize) -> GyResult<Self> {
        create_and_ensure_length(path, size).map_err(|err| err.to_string())?;
        let mmap = open_write_mmap(path, false).map_err(|err| err.to_string())?;
        let path = path.to_path_buf();
        Ok(Page { path, mmap })
    }
}
