use super::schema::BinarySerialize;
use super::schema::{DocID, Document};
use super::util::error::{GyError, GyResult};
use super::util::fs;
use super::util::fst::{FstBuilder, FstReader};
use super::IndexReader;
use crate::gypaetus::schema::VUInt;
use crate::gypaetus::schema::VarIntSerialize;
use crate::gypaetus::DocFreq;
use crate::gypaetus::Term;
use crate::iocopy;
use art_tree::Key;
use bytes::Buf;
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
};
use varintrs::{Binary, WriteBytesVarExt};
const KB: usize = 1 << 10;
const MB: usize = KB * KB;
const DEFAULT_BLOCK_SIZE: usize = 4 * KB + KB;
const FOOTER_LEN: u64 = 48;

const MAGIC: &'static [u8] = b"\xD4\x56\x3F\x35\xE0\xEF\x09\x7A";

//压缩类型
enum CompressionType {
    No,
    LZ4,
    LowercaseAscii,
    Zstandard,
    Snappy,
}

//  +---------------------+
//  |                     |
//  |       Doc           |
//  |                     |
//  +---------------------+
//  |                     |
//  |       vector        |
//  |                     |
//  +---------------------+
//  |       ......        |
//  +---------------------+
//  |                     |
//  |       Posting       |
//  |                     |
//  +---------------------+
//  |                     |
//  |       Bloom         |
//  |                     |
//  +---------------------+
//  |                     |
//  |       Fst           |
//  |                     |
//  +---------------------+
//  |       ......        |
//  +---------------------+
//  |                     |
//  |       doc_meta      |
//  |                     |
//  +---------------------+
//  |                     |
//  |       field_meta    |
//  |                     |
//  +---------------------+
//  |                     |
//  |       footer        |
//  |                     |
//  +---------------------+

//合并索引
pub fn flush_index(reader: &IndexReader) -> GyResult<()> {
    let mut writer = DiskStoreWriter::with_offset(
        reader.get_index_config().get_wal_fname(),
        reader.offset()?,
        reader.reader.doc_offset.read()?.len(),
    )?;
    let mut buf = Vec::with_capacity(4 * KB);
    //写入文档位置信息

    //写入每个域的信息
    for field in reader.iter() {
        let mut term_offset_cache: HashMap<Vec<u8>, usize> = HashMap::new();
        for (b, p) in field.indexs.read()?.iter() {
            let (start_addr, end_addr) = {
                let posting = (*p).read()?;
                (posting.byte_addr.clone(), posting.doc_freq_addr.clone())
            };
            let posting_buffer = field.posting_buffer(start_addr, end_addr)?;
            //写入倒排表
            for rb in posting_buffer.iter() {
                buf.write(rb)?;
            }
            let offset = writer.write_posting(buf.as_slice())?;
            term_offset_cache.insert(b.to_bytes(), offset);
            buf.clear();
        }
        let mut term_vec: Vec<(&Vec<u8>, &usize)> = term_offset_cache.iter().collect();
        //排序
        term_vec.sort_by(|a, b| a.1.cmp(b.1));
        // 写入字典索引
        for (t, u) in term_vec {
            println!("{:?}", t);
            writer.add_term(t, *u)?;
        }
        writer.finish_field()?;
    }
    // 写入文档和偏移量关系 meta
    writer.write_doc_meta(&reader.get_doc_offset().read()?)?;
    // 写入每个域的 meta
    writer.write_field_meta()?;
    writer.close()?;

    Ok(())
}

pub fn merge() {}

pub struct DiskStoreReader {
    fields_meta: Vec<FieldHandle>,
    doc_meta: Vec<usize>,
    file: File,
    mmap: Arc<Mmap>,
}

impl DiskStoreReader {
    pub fn open<P: AsRef<Path>>(fname: P) -> GyResult<DiskStoreReader> {
        let mut file = OpenOptions::new().read(true).open(fname)?;
        let file_size = file.metadata()?.size();
        if file_size < FOOTER_LEN {}
        let footer_pos = file_size - FOOTER_LEN;
        let mut footer = [0u8; FOOTER_LEN as usize];
        file.read_at(&mut footer, footer_pos)?;
        //判断魔数是否正确
        if &footer[FOOTER_LEN as usize - MAGIC.len()..FOOTER_LEN as usize] != MAGIC {
            return Err(GyError::ErrBadMagicNumber);
        }
        let mut c = std::io::Cursor::new(&footer[..]);
        let doc_meta_bh: BlockHandle = BlockHandle::deserialize(&mut c)?;
        let field_meta_bh = BlockHandle::deserialize(&mut c)?;

        let mmap: Mmap = unsafe {
            memmap2::MmapOptions::new()
                .map(&file)
                .map_err(|e| format!("mmap failed: {}", e))?
        };
        let fields_meta = Self::read_at_bh::<Vec<FieldHandle>>(&mmap, field_meta_bh)?;
        let doc_meta = Self::read_at_bh::<Vec<usize>>(&mmap, doc_meta_bh)?;

        Ok(Self {
            fields_meta: fields_meta, //fields_meta,
            doc_meta: doc_meta,       //doc_meta,
            file: file,
            mmap: Arc::new(mmap),
        })
    }

    fn read_at_bh<T: BinarySerialize>(mmap: &Mmap, bh: BlockHandle) -> GyResult<T> {
        let mut r = mmap[bh.start()..bh.end()].reader();
        let v = T::deserialize(&mut r)?;
        Ok(v)
    }

    fn read_at<T: BinarySerialize>(&self, offset: usize) -> GyResult<T> {
        let mut r = self.mmap[offset..].reader();
        let v = T::deserialize(&mut r)?;
        Ok(v)
    }

    pub fn field_reader<'a>(&'a self, field_id: u32) -> GyResult<DiskFieldReader<'a>> {
        let field_handle = &self.fields_meta[field_id as usize];
        println!(
            "get fst:{:?}",
            &self.mmap[field_handle.fst_bh.start()..field_handle.fst_bh.end()]
        );
        let fst =
            FstReader::load(&self.mmap[field_handle.fst_bh.start()..field_handle.fst_bh.end()]);
        Ok(DiskFieldReader {
            fst: fst,
            mmap: self.mmap.clone(),
        })
    }

    pub fn search(&self, term: Term) -> GyResult<DiskPostingReader> {
        let field_id = term.field_id().0;
        let field_reader = self.field_reader(field_id)?;
        field_reader.get(term.bytes_value())
    }

    pub(crate) fn doc(&self, doc_id: DocID) -> GyResult<Document> {
        let doc_offset = self.doc_meta[doc_id as usize];
        let doc = self.read_at::<Document>(doc_offset)?;
        Ok(doc)
    }
}

pub struct DiskFieldReader<'a> {
    fst: FstReader<'a>,
    mmap: Arc<Mmap>,
}

impl<'a> DiskFieldReader<'a> {
    fn get(&self, term: &[u8]) -> GyResult<DiskPostingReader> {
        let offset = self.fst.get(term)?;
        Ok(DiskPostingReader::new(self.mmap.clone(), offset as usize)?)
    }
}

pub struct DiskPostingReader {
    last_docid: DocID,
    snapshot: DiskSnapshotReader,
}

impl DiskPostingReader {
    fn new(mmap: Arc<Mmap>, offset: usize) -> GyResult<DiskPostingReader> {
        Ok(DiskPostingReader {
            last_docid: 0,
            snapshot: DiskSnapshotReader::new(mmap, offset)?,
        })
    }
}

impl Iterator for DiskPostingReader {
    type Item = DocFreq;
    fn next(&mut self) -> Option<Self::Item> {
        return match DocFreq::deserialize(&mut self.snapshot) {
            Ok(mut doc_freq) => {
                self.last_docid += doc_freq.doc() >> 1;
                doc_freq.0 = self.last_docid;
                Some(doc_freq)
            }
            Err(_) => None,
        };
    }
}

struct DiskSnapshotReader {
    mmap: Arc<Mmap>,
    offset: usize,
    end: usize,
}

impl DiskSnapshotReader {
    fn new(mmap: Arc<Mmap>, mut offset: usize) -> GyResult<DiskSnapshotReader> {
        let mut r = mmap[offset..].reader();
        let (length, i) = VUInt::deserialize(&mut r)?;
        let l = length.val() as usize;
        println!(
            "length:{},p:{:?},offset:{}",
            length.val(),
            &mmap[offset..offset + l],
            offset
        );
        offset += i;
        let snapshot = Self {
            mmap: mmap,
            offset: offset,
            end: offset + l,
        };
        Ok(snapshot)
    }
}

impl Read for DiskSnapshotReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.offset >= self.end {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "io EOF",
            ));
        }
        let i = iocopy!(buf, &self.mmap[self.offset..]);
        self.offset += i;
        Ok(i)
    }
}

struct DiskSnapshotReaderIter {
    mmap: Arc<Mmap>,
    offset: usize,
    lenght: usize,
}

pub struct DiskStoreWriter {
    offset: usize,
    filter_block: FilterBlock, //布隆过滤器
    index_block: IndexBlock,   //fst
    field_bhs: Vec<FieldHandle>,
    doc_meta_bh: BlockHandle,
    field_meta_bh: BlockHandle,
    file: File,
}

#[derive(Debug)]
struct FieldHandle {
    fst_bh: BlockHandle,
    filter_bh: BlockHandle,
}

impl BinarySerialize for FieldHandle {
    fn serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        self.fst_bh.serialize(writer)?;
        self.filter_bh.serialize(writer)
    }

    fn deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let fst_bh = BlockHandle::deserialize(reader)?;
        let filter_bh = BlockHandle::deserialize(reader)?;
        Ok(FieldHandle { fst_bh, filter_bh })
    }
}

#[derive(Default, Debug)]
struct BlockHandle(usize, usize);

impl BlockHandle {
    fn start(&self) -> usize {
        self.0
    }

    fn end(&self) -> usize {
        self.0 + self.1
    }
}

impl BinarySerialize for BlockHandle {
    fn serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        VUInt(self.0 as u64).serialize(writer)?;
        VUInt(self.1 as u64).serialize(writer)?;
        Ok(())
    }

    fn deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let u1 = VUInt::deserialize(reader)?.0.val();
        let u2 = VUInt::deserialize(reader)?.0.val();
        Ok(BlockHandle(u1 as usize, u2 as usize))
    }
}

impl DiskStoreWriter {
    fn new(fname: &Path) -> GyResult<DiskStoreWriter> {
        let file = fs::open_file(fname, true, true)?;
        let w = DiskStoreWriter {
            offset: 0,
            filter_block: FilterBlock::new(),
            index_block: IndexBlock::new(),
            field_bhs: Vec::new(),
            doc_meta_bh: BlockHandle::default(),
            field_meta_bh: BlockHandle::default(),
            file: file,
        };
        Ok(w)
    }

    fn with_offset(fname: &Path, offset: usize, length: usize) -> GyResult<DiskStoreWriter> {
        let mut w = DiskStoreWriter::new(fname)?;
        w.seek(offset as u64)?;
        w.offset = offset;
        Ok(w)
    }

    fn seek(&mut self, offset: u64) -> GyResult<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        Ok(())
    }

    fn get_cursor(&mut self) -> GyResult<u64> {
        let cursor = self.file.seek(SeekFrom::Current(0))?;
        Ok(cursor)
    }

    fn add_term(&mut self, key: &[u8], offset: usize) -> GyResult<()> {
        self.index_block.add(key, offset as u64)?;
        Ok(())
    }

    fn write_doc_meta(&mut self, meta: &[usize]) -> GyResult<()> {
        let offset = self.offset;
        println!("doc_meta:{}", offset);
        meta.serialize(self)?;
        self.flush()?;
        self.offset = self.get_cursor()? as usize;
        println!("doc_meta cursor:{}", self.offset);
        self.doc_meta_bh = BlockHandle(offset, self.offset - offset);
        Ok(())
    }

    fn write_field_meta(&mut self) -> GyResult<()> {
        let offset = self.offset;
        println!("field_meta:{}", offset);
        self.field_bhs.serialize(&mut self.file)?;
        self.flush()?;
        self.offset = self.get_cursor()? as usize;
        println!("field_meta cursor:{}", self.offset);
        self.field_meta_bh = BlockHandle(offset, self.offset - offset);
        Ok(())
    }

    fn finish_field(&mut self) -> GyResult<()> {
        let bh = self.finish_index_block()?;
        self.field_bhs.push(FieldHandle {
            fst_bh: bh,
            filter_bh: BlockHandle::default(),
        });
        Ok(())
    }

    fn close(&mut self) -> GyResult<()> {
        let mut footer = [0u8; FOOTER_LEN as usize];
        let mut c = std::io::Cursor::new(&mut footer[..]);
        self.doc_meta_bh.serialize(&mut c)?;
        self.field_meta_bh.serialize(&mut c)?;
        iocopy!(&mut footer[FOOTER_LEN as usize - MAGIC.len()..], MAGIC);
        self.write(&footer)?;
        self.flush()?;
        self.truncate()?;
        Ok(())
    }

    fn truncate(&mut self) -> GyResult<()> {
        let cursor = self.get_cursor()?;
        self.file.set_len(cursor)?;
        Ok(())
    }

    fn finish_index_block(&mut self) -> GyResult<BlockHandle> {
        let length = self.index_block.finish()?;
        let offset = self.offset;
        self.index_block.serialize(&mut self.file)?;
        self.index_block.reset()?;
        self.flush()?;
        self.offset = offset + length;
        Ok(BlockHandle(offset, length))
    }

    fn write_posting(&mut self, b: &[u8]) -> GyResult<usize> {
        let offset = self.offset;
        VUInt(b.len() as u64).serialize(&mut self.file)?;
        println!("len:{},u8:{:?},offset:{}", b.len(), b, offset);
        self.file.write(b)?;
        self.flush()?;
        self.offset = self.get_cursor()? as usize;
        Ok(offset)
    }
}

impl Write for DiskStoreWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
#[derive(Default)]
struct FilterBlock {}

impl FilterBlock {
    fn new() -> FilterBlock {
        FilterBlock {}
    }
}

struct IndexBlock {
    fst: FstBuilder,
}

impl IndexBlock {
    fn new() -> IndexBlock {
        IndexBlock {
            fst: FstBuilder::new(),
        }
    }

    fn add(&mut self, key: &[u8], val: u64) -> GyResult<()> {
        self.fst.add(key, val)
    }

    fn finish(&mut self) -> GyResult<usize> {
        self.fst.finish()?;
        Ok(self.fst.get_ref().len())
    }

    fn reset(&mut self) -> GyResult<()> {
        self.fst.reset()
    }
}

impl BinarySerialize for IndexBlock {
    fn serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write(self.fst.get_ref())?;
        writer.flush()?;
        Ok(())
    }

    fn deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        todo!()
    }
}

struct IndexBlockReader {}

struct DataBlock {
    buf: Vec<u8>,
}

#[inline]
fn shared_prefix_len(a: &[u8], b: &[u8]) -> usize {
    let (mut i, mut n) = (0 as usize, a.len());
    if n > b.len() {
        n = b.len()
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    return i;
}

impl DataBlock {
    fn new() -> DataBlock {
        Self {
            buf: Vec::with_capacity(DEFAULT_BLOCK_SIZE),
        }
    }

    fn reset(&mut self) {
        self.buf.clear();
    }

    // fn get(&mut self) -> GyResult<(&[u8], &[u8])> {
    //     self.buf1.write_vu64::<Binary>(self.buf2.len() as u64)?;
    //     Ok((self.buf1.as_slice(), self.buf2.as_slice()))
    // }

    fn write(&mut self, key: &[u8]) -> GyResult<()> {
        self.buf.write(key)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;
    use varintrs::{Binary, WriteBytesVarExt};
    #[test]
    fn test_bytes() {
        let mut buffer: Vec<u8> = Vec::with_capacity(10);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        //buffer.write_vi64::<Binary>(1024);
        println!("buf1:{:?}", buffer);
        buffer.clear();

        buffer.write_vi64::<Binary>(2048);
        buffer.write_vi64::<Binary>(2048);

        println!("buf2:{:?}", buffer);
        // buffer.put_f32(n)
    }

    #[test]
    fn test_disk_reader() {
        let mut buffer: Vec<u8> = Vec::with_capacity(10);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        buffer.write_vi64::<Binary>(1024);
        //buffer.write_vi64::<Binary>(1024);
        println!("buf1:{:?}", buffer);
        buffer.clear();

        buffer.write_vi64::<Binary>(2048);
        buffer.write_vi64::<Binary>(2048);

        println!("buf2:{:?}", buffer);
        // buffer.put_f32(n)
    }
}
