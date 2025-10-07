use memmap2::MmapMut;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

pub(crate) const TEMP_FILE_EXTENSION: &str = "tmp";

pub(crate) fn create_and_ensure_length(path: &Path, length: usize) -> io::Result<File> {
    if path.exists() {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.set_len(length as u64)?;
        Ok(file)
    } else {
        let temp_path = path.with_extension(TEMP_FILE_EXTENSION);
        {
            let temp_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&temp_path)?;
            temp_file.set_len(length as u64)?;
        }

        std::fs::rename(&temp_path, path)?;

        OpenOptions::new().read(true).write(true).open(path)
    }
}

pub fn open_write_mmap(path: &Path, populate: bool) -> io::Result<MmapMut> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let mmap = if populate {
        unsafe { memmap2::MmapOptions::new().populate().map_mut(&file)? }
    } else {
        unsafe { memmap2::MmapOptions::new().map_mut(&file)? }
    };
    Ok(mmap)
}
