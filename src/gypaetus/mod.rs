mod ann;
mod buffer;
mod disk;
mod query;
mod schema;
mod searcher;
mod tokenize;
mod util;
use art_tree::{Art, ByteString, Key};
use query::Term;
use util::error::{GyError, GyResult};
mod macros;
pub mod wal;
use ann::BoxedAnnIndex;
use ann::{Create, Metric, HNSW};
use buffer::{
    Addr, ByteBlockPool, RingBuffer, RingBufferReader, SnapshotReader, SnapshotReaderIter,
    BLOCK_SIZE_CLASS,
};

use schema::{BinarySerialize, DocID, Document, Schema, Value, VectorType};

//use jiebars::Jieba;
use lock_api::RawMutex;
use parking_lot::Mutex;
use std::cell::RefCell;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, Weak};
use wal::{IOType, Wal, DEFAULT_WAL_FILE_SIZE};

use crate::gypaetus::wal::WalReader;

use self::schema::DocFreq;

const META_FILE: &'static str = "meta.json"; // index 元数据
const DELETE_FILE: &'static str = "ids.del"; // 被删除的id

pub struct IndexConfigBuilder {
    index_name: String,
    io_type: IOType,
    index_path: PathBuf,
    wal_fname: PathBuf,
    fsize: usize,
}

impl Default for IndexConfigBuilder {
    fn default() -> IndexConfigBuilder {
        IndexConfigBuilder {
            index_name: String::default(),
            io_type: IOType::MMAP,
            index_path: PathBuf::from("./"),
            wal_fname: PathBuf::from("./000000.wal"),
            fsize: DEFAULT_WAL_FILE_SIZE,
        }
    }
}

impl IndexConfigBuilder {
    pub fn index_name(mut self, index_name: String) -> IndexConfigBuilder {
        self.index_name = index_name;
        self
    }

    pub fn io_type(mut self, io_type: IOType) -> IndexConfigBuilder {
        self.io_type = io_type;
        self
    }

    pub fn index_path(mut self, index_path: PathBuf) -> IndexConfigBuilder {
        self.index_path = index_path;
        self
    }

    pub fn wal_fname(mut self, wal_fname: PathBuf) -> IndexConfigBuilder {
        self.wal_fname = wal_fname;
        self
    }

    pub fn fsize(mut self, fsize: usize) -> IndexConfigBuilder {
        self.fsize = fsize;
        self
    }

    pub fn build(self) -> IndexConfig {
        IndexConfig {
            index_name: self.index_name,
            io_type: self.io_type,
            index_path: self.index_path,
            wal_fname: self.wal_fname,
            fsize: self.fsize,
        }
    }
}

pub struct IndexConfig {
    index_name: String,
    io_type: IOType,
    index_path: PathBuf,
    wal_fname: PathBuf,
    fsize: usize,
}

impl IndexConfig {
    pub fn get_index_name(&self) -> &str {
        &self.index_name
    }

    pub fn get_io_type(&self) -> &IOType {
        &self.io_type
    }

    pub fn get_index_path(&self) -> &Path {
        &self.index_path
    }

    pub fn get_wal_fname(&self) -> &Path {
        &self.wal_fname
    }

    pub fn get_fsize(&self) -> usize {
        self.fsize
    }
}

unsafe impl Send for Index {}
unsafe impl Sync for Index {}

#[derive(Clone)]
// 实现基础搜索能力
pub struct Index(Arc<IndexBase>);

impl Index {
    pub fn new(schema: Schema, config: IndexConfig) -> GyResult<Index> {
        Ok(Self(Arc::new(IndexBase::new(schema, config)?)))
    }

    pub fn reader(&self) -> GyResult<IndexReader> {
        Ok(IndexReader::new(self.0.clone()))
    }

    pub fn writer(&mut self) -> GyResult<IndexWriter> {
        Ok(IndexWriter::new(self.0.clone()))
    }

    pub fn create_vector_collection<V>(self, vector: VectorType) -> Collection<V>
    where
        V: Metric<V> + Create,
    {
        Collection::new(self, &vector)
    }

    pub fn close() {}
}

//时序搜索
// pub struct Series {
//     series: HashMap<SeriesID>,
// }

//impl Series {}

unsafe impl<V: 'static> Send for Collection<V> where V: Metric<V> + Create {}
unsafe impl<V: 'static> Sync for Collection<V> where V: Metric<V> + Create {}

//向量搜索
pub struct Collection<V: 'static>
where
    V: Metric<V> + Create,
{
    vector_field: RwLock<BoxedAnnIndex<V>>,
    index: Index,
    rw_lock: Mutex<()>,
}

impl<V: 'static> Collection<V>
where
    V: Metric<V> + Create,
{
    fn new(index: Index, vec_type: &VectorType) -> Collection<V> {
        Collection {
            vector_field: RwLock::new(Self::get_vector_index(&vec_type).unwrap()),
            index: index,
            rw_lock: Mutex::new(()),
        }
    }

    pub fn get_vector_index(vec_type: &VectorType) -> Option<BoxedAnnIndex<V>> {
        match vec_type {
            VectorType::HNSW => Some(BoxedAnnIndex(Box::new(HNSW::<V>::new(32)))),
            _ => None,
        }
    }

    pub fn add(&mut self) -> GyResult<()> {
        todo!()
    }

    pub fn add_with_raw(&mut self, content: &[u8]) -> GyResult<()> {
        todo!()
        // unsafe {
        //     self.rw_lock.raw().lock();
        // }
        // self.index.0.write_wal(content)?;
        // self.index.0.inner_add(&vec.d)?;
        // self.vector_field
        //     .write()?
        //     .0
        //     .insert(vec.into(), *self.index.0.doc_id.read().unwrap() as usize);
        // *self.index.0.doc_id.write()? += 1;
        // Ok(())
    }

    pub fn flush() {}
}

unsafe impl Send for IndexBase {}
unsafe impl Sync for IndexBase {}

pub struct IndexBase {
    schema: Schema,
    fields: Vec<FieldCache>,
    doc_id: RefCell<DocID>,
    buffer: Arc<RingBuffer>,
    wal: Arc<RwLock<Wal>>,
    doc_offset: RwLock<Vec<usize>>,
    rw_lock: Mutex<()>,
    config: IndexConfig,
}

impl IndexBase {
    fn new(schema: Schema, config: IndexConfig) -> GyResult<IndexBase> {
        let index_path = config.get_index_path().join(config.get_index_name());

        let buffer_pool = Arc::new(RingBuffer::new());
        let mut field_cache: Vec<FieldCache> = Vec::new();
        for _ in 0..schema.fields.len() {
            field_cache.push(FieldCache::new(Arc::downgrade(&buffer_pool)));
        }
        Ok(Self {
            fields: field_cache,
            doc_id: RefCell::new(0),
            buffer: buffer_pool,
            schema: schema,
            rw_lock: Mutex::new(()),
            wal: Arc::new(RwLock::new(Wal::new(
                &config.wal_fname,
                config.fsize,
                config.io_type,
            )?)),
            doc_offset: RwLock::new(Vec::with_capacity(1024)), // Max memory doc
            config: config,
        })
    }

    fn reload() -> GyResult<()> {
        Ok(())
    }

    fn get_config(&self) -> &IndexConfig {
        &self.config
    }

    fn inner_add(&self, doc: &Document) -> GyResult<()> {
        for field in doc.field_values.iter() {
            println!("field.field_id().0:{}", field.field_id().0);
            let fw = self.fields.get(field.field_id().0 as usize).unwrap();
            fw.add(*self.doc_id.borrow(), field.value())?;
        }
        Ok(())
    }

    pub fn write_wal(&self, content: &[u8]) -> GyResult<usize> {
        self.wal.read()?.check_rotate(content.len())?;
        let mut w = self.wal.write()?;
        w.write_bytes(content)?;
        w.flush()?;
        Ok(w.offset())
    }

    pub fn write_doc_to_wal(&self, doc: &Document) -> GyResult<usize> {
        {
            self.wal.read()?.check_rotate(doc.size())?;
        }
        let offset = {
            let mut w = self.wal.write()?;
            let offset = w.offset();
            doc.serialize(&mut *w)?;
            w.flush()?;
            drop(w);
            offset
        };
        Ok(offset)
    }

    pub fn add_with_raw(&mut self, content: &[u8]) -> GyResult<()> {
        unsafe {
            self.rw_lock.raw().lock();
        }
        let doc_offset = self.write_wal(content)?;
        {
            self.doc_offset.write()?.push(doc_offset);
        }
        let mut cursor = Cursor::new(content);
        let doc = Document::deserialize(&mut cursor)?;
        self.inner_add(&doc)?;
        //添加向量
        *self.doc_id.borrow_mut() += 1;
        self.commit()?;

        unsafe {
            self.rw_lock.raw().unlock();
        }
        Ok(())
    }

    //add vector
    pub fn add(&self, doc: &Document) -> GyResult<()> {
        unsafe {
            self.rw_lock.raw().lock();
        }
        let doc_offset = self.write_doc_to_wal(doc)?;
        {
            self.doc_offset.write()?.push(doc_offset);
        }
        self.inner_add(doc)?;
        self.commit()?;
        *self.doc_id.borrow_mut() += 1;
        unsafe {
            self.rw_lock.raw().unlock();
        }
        Ok(())
    }

    pub fn field_reader(&self, field_id: u32) -> GyResult<FieldReader> {
        Ok(self.fields[field_id as usize].reader())
    }

    // commit 之后文档能搜索得到
    fn commit(&self) -> GyResult<()> {
        self.fields
            .iter()
            .try_for_each(|field| -> Result<(), std::io::Error> {
                field
                    .commit()
                    .map_err(|e| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
                Ok(())
            })?;
        Ok(())
    }

    fn doc_offset(&self, doc_id: DocID) -> GyResult<usize> {
        let offset = {
            let doc_offset = self.doc_offset.read()?;
            if doc_id as usize >= doc_offset.len() {
                return Err(GyError::ErrDocumentNotFound);
            }
            let offset = doc_offset.get(doc_id as usize).unwrap().clone();
            drop(doc_offset);
            offset
        };
        Ok(offset)
    }

    // 手动flush到磁盘中
    fn flush(&mut self, path: PathBuf) {}
}

type Posting = Arc<RwLock<_Posting>>;

// 倒排表
pub struct _Posting {
    last_doc_id: DocID,
    doc_delta: DocID,
    byte_addr: Addr,
    doc_freq_addr: Addr,
    doc_num: usize,
    freq: u32,
    add_commit: bool,
}

impl _Posting {
    fn new(doc_freq_addr: Addr, pos_addr: Addr) -> _Posting {
        Self {
            last_doc_id: 0,
            doc_delta: 0,
            byte_addr: doc_freq_addr,
            doc_freq_addr: doc_freq_addr,
            doc_num: 0,
            add_commit: false,
            freq: 0,
        }
    }
}

// 用于数值索引
pub(crate) struct TrieIntCache {}

//用于字符串索引
pub(crate) struct ArtCache {
    cache: Art<ByteString, Posting>,
}

impl ArtCache {
    pub(crate) fn new() -> ArtCache {
        Self { cache: Art::new() }
    }
}

impl ArtCache {
    fn contains_key(&self, k: &[u8]) -> bool {
        let a = self.cache.iter();
        match self.cache.get(&ByteString::new(&k)) {
            Some(_) => true,
            None => false,
        }
    }

    fn insert(&mut self, k: Vec<u8>, v: Posting) -> Option<Posting> {
        self.cache.upsert(ByteString::new(&k), v.clone());
        Some(v)
    }

    fn get(&self, k: &[u8]) -> Option<&Posting> {
        self.cache.get(&ByteString::new(&k))
    }

    fn iter(&self) -> impl DoubleEndedIterator<Item = (&ByteString, &Posting)> {
        self.cache.iter()
    }
}

pub(crate) struct FieldCache {
    indexs: Arc<RwLock<ArtCache>>,
    share_bytes_block: Weak<RingBuffer>,
    commit_posting: RefCell<Vec<Posting>>,
}

impl FieldCache {
    fn new(pool: Weak<RingBuffer>) -> FieldCache {
        Self {
            indexs: Arc::new(RwLock::new(ArtCache::new())),
            share_bytes_block: pool,
            commit_posting: RefCell::new(Vec::new()),
        }
    }

    fn reader(&self) -> FieldReader {
        FieldReader::new(self.indexs.clone(), self.share_bytes_block.clone())
    }

    fn commit(&self) -> GyResult<()> {
        let pool = self.share_bytes_block.upgrade().unwrap();
        self.commit_posting
            .borrow_mut()
            .iter()
            .try_for_each(|posting| -> GyResult<()> {
                let p = &mut posting.write().unwrap();
                Self::write_doc_freq(p.doc_delta, p, &mut *pool.borrow_mut())?;
                p.add_commit = false;
                p.freq = 0;
                Ok(())
            })?;
        self.commit_posting.borrow_mut().clear();
        Ok(())
    }

    //添加 token 单词
    pub fn add(&self, doc_id: DocID, value: &Value) -> GyResult<()> {
        let v = value.to_vec()?;
        if !self.indexs.read()?.contains_key(&v) {
            let pool = self.share_bytes_block.upgrade().unwrap();
            let pos = (*pool).borrow_mut().alloc_bytes(0, None);
            self.indexs.write()?.insert(
                v.clone(),
                Arc::new(RwLock::new(_Posting::new(pos, pos + BLOCK_SIZE_CLASS[1]))),
            );
        }
        // 获取词典的倒排表
        let p: Posting = self
            .indexs
            .read()?
            .get(&v)
            .expect("get term posting list fail")
            .clone();
        // 获取bytes 池
        let pool = self.share_bytes_block.upgrade().unwrap();
        // 倒排表中加入文档id
        Self::add_doc(doc_id, &mut *p.write()?, &mut *pool.borrow_mut())?;
        if !(*p).read()?.add_commit {
            self.commit_posting.borrow_mut().push(p.clone());
            (*p).write()?.add_commit = true;
        }

        Ok(())
    }

    // 添加vec
    fn add_doc(
        doc_id: DocID,
        posting: &mut _Posting,
        block_pool: &mut ByteBlockPool,
    ) -> GyResult<()> {
        if !posting.add_commit {
            posting.doc_num += 1;
            posting.doc_delta = doc_id - posting.last_doc_id;
            posting.freq += 1;
            posting.last_doc_id = doc_id;
        } else if posting.last_doc_id == doc_id {
            posting.freq += 1;
        } else {
            Self::write_doc_freq(posting.doc_delta, posting, block_pool)?;
            posting.doc_delta = doc_id - posting.last_doc_id;
            posting.last_doc_id = doc_id;
            posting.doc_num += 1;
            posting.freq = 1;
        }
        Ok(())
    }

    fn write_doc_freq(
        doc_delta: DocID,
        posting: &mut _Posting,
        block_pool: &mut ByteBlockPool,
    ) -> GyResult<()> {
        block_pool.set_pos(posting.doc_freq_addr);
        DocFreq(doc_delta, posting.freq).serialize(block_pool)?;
        posting.doc_freq_addr = block_pool.get_pos();
        Ok(())
    }

    fn add_pos(
        pos: usize,
        posting: &mut Posting,
        pool: Arc<RefCell<ByteBlockPool>>,
    ) -> GyResult<()> {
        Ok(())
    }
}

pub struct FieldReader {
    indexs: Arc<RwLock<ArtCache>>,
    share_bytes_block: Weak<RingBuffer>,
}

impl FieldReader {
    fn new(indexs: Arc<RwLock<ArtCache>>, share_bytes_block: Weak<RingBuffer>) -> FieldReader {
        Self {
            indexs: indexs,
            share_bytes_block: share_bytes_block,
        }
    }

    fn posting_buffer(&self, start_addr: Addr, end_addr: Addr) -> GyResult<RingBufferReader> {
        Ok(RingBufferReader::new(
            self.share_bytes_block.upgrade().unwrap(),
            start_addr,
            end_addr,
        ))
    }

    fn posting(&self, start_addr: Addr, end_addr: Addr) -> GyResult<PostingReader> {
        let reader = RingBufferReader::new(
            self.share_bytes_block.upgrade().unwrap(),
            start_addr,
            end_addr,
        );
        let r = SnapshotReader::new(reader);
        Ok(PostingReader::new(r))
    }

    fn get(&self, term: &[u8]) -> GyResult<PostingReader> {
        let (start_addr, end_addr) = {
            let index = self.indexs.read()?;
            let posting = index.get(term).unwrap();
            let (start_addr, end_addr) = (
                (*posting).read()?.byte_addr.clone(),
                (*posting).read()?.doc_freq_addr.clone(),
            );
            (start_addr, end_addr)
        };
        self.posting(start_addr, end_addr)
    }
}

pub struct PostingReader {
    snap: SnapshotReader,
}

impl PostingReader {
    fn new(snap: SnapshotReader) -> PostingReader {
        PostingReader { snap: snap }
    }

    pub fn iter<'a>(&'a self) -> PostingReaderIter<'a> {
        PostingReaderIter {
            last_docid: 0,
            snap_iter: self.snap.iter(),
        }
    }
}

pub struct PostingReaderIter<'a> {
    last_docid: DocID,
    snap_iter: SnapshotReaderIter<'a>,
}

impl<'b, 'a> Iterator for PostingReaderIter<'a> {
    type Item = DocFreq;
    fn next(&mut self) -> Option<Self::Item> {
        return match DocFreq::deserialize(&mut self.snap_iter) {
            Ok(mut doc_freq) => {
                self.last_docid += doc_freq.doc() >> 1;
                doc_freq.0 = self.last_docid;
                Some(doc_freq)
            }
            Err(_) => None,
        };
    }
}

pub struct IndexReaderIter {
    reader: Arc<IndexBase>,
    i: usize,
}

impl IndexReaderIter {
    fn new(reader: Arc<IndexBase>) -> IndexReaderIter {
        Self {
            reader: reader,
            i: 0,
        }
    }
}

impl Iterator for IndexReaderIter {
    type Item = FieldReader;
    fn next(&mut self) -> Option<Self::Item> {
        if self.i >= self.reader.fields.len() {
            return None;
        }
        let r = self.reader.fields[self.i].reader();
        self.i += 1;
        Some(r)
    }
}

pub struct IndexReader {
    reader: Arc<IndexBase>,
    wal: Arc<RwLock<Wal>>,
}

impl IndexReader {
    fn new(reader: Arc<IndexBase>) -> IndexReader {
        let wal = reader.wal.clone();
        IndexReader {
            reader: reader,
            wal: wal,
        }
    }

    pub fn get_index_config(&self) -> &IndexConfig {
        self.reader.get_config()
    }

    pub fn iter(&self) -> IndexReaderIter {
        IndexReaderIter::new(self.reader.clone())
    }

    fn search(&self, term: Term) -> GyResult<PostingReader> {
        let field_id = term.field_id().0;
        let field_reader = self.reader.field_reader(field_id)?;
        field_reader.get(term.bytes_value())
    }

    pub(crate) fn doc(&self, doc_id: DocID) -> GyResult<Document> {
        let doc_offset = self.reader.doc_offset(doc_id)?;
        let doc: Document = {
            let mut wal = self.wal.read()?;
            let mut wal_read = WalReader::from(&mut wal, doc_offset);
            Document::deserialize(&mut wal_read)?
        };
        Ok(doc)
    }

    pub(crate) fn offset(&self) -> GyResult<usize> {
        let i = self.wal.read()?.offset();
        Ok(i)
    }

    pub(crate) fn get_doc_offset(&self) -> &RwLock<Vec<usize>> {
        &self.reader.doc_offset
    }
}

pub struct IndexWriter {
    writer: Arc<IndexBase>,
}

impl IndexWriter {
    fn new(writer: Arc<IndexBase>) -> IndexWriter {
        IndexWriter { writer: writer }
    }

    pub fn add(&mut self, doc: &Document) -> GyResult<()> {
        self.writer.add(doc)
    }

    pub fn commit(&mut self) -> GyResult<()> {
        self.writer.commit()
    }
}

#[cfg(test)]
mod tests {

    use super::{schema::FieldEntry, *};
    use schema::BinarySerialize;
    use std::thread;
    use tests::disk::DiskStoreReader;

    #[test]
    fn test_add_doc() {
        let mut schema = Schema::new();
        schema.add_field(FieldEntry::str("body"));
        schema.add_field(FieldEntry::i32("title"));
        let field_id_title = schema.get_field("title").unwrap();
        let config = IndexConfigBuilder::default().build();
        let mut index = Index::new(schema, config).unwrap();
        let mut writer1 = index.writer().unwrap();

        let mut d = Document::new();
        d.add_text(field_id_title.clone(), "bb");
        writer1.add(&d).unwrap();

        let mut d1 = Document::new();
        d1.add_text(field_id_title.clone(), "aa");
        writer1.add(&d1).unwrap();

        let reader = index.reader().unwrap();
        let p = reader
            .search(Term::from_field_text(field_id_title, "aa"))
            .unwrap();

        for doc_freq in p.iter() {
            let doc = reader.doc(doc_freq.doc()).unwrap();
            println!("docid:{},doc{:?}", doc_freq.doc(), doc);
        }
        println!("doc vec:{:?}", reader.get_doc_offset().read().unwrap());
        disk::flush_index(&reader).unwrap();

        // let disk_reader =
        //     DiskStoreReader::open("/opt/rsproject/gptgrep/searchlite/0000000.wal");
    }

    #[test]
    fn test_read() {
        let mut schema = Schema::new();
        schema.add_field(FieldEntry::str("body"));
        schema.add_field(FieldEntry::i32("title"));
        let field_id_title = schema.get_field("title").unwrap();
        let disk_reader =
            DiskStoreReader::open("/opt/rsproject/gptgrep/searchlite/000000.wal").unwrap();
        let p = disk_reader
            .search(Term::from_field_text(field_id_title, "aa"))
            .unwrap();
        for doc_freq in p {
            println!("{:?}", doc_freq);
            // let doc = disk_reader.doc(doc_freq.doc()).unwrap();
            //  println!("docid:{},doc{:?}", doc_freq.doc(), doc);
        }
    }

    #[test]
    fn test_iter() {
        let mut schema = Schema::new();
        schema.add_field(FieldEntry::str("body"));
        schema.add_field(FieldEntry::i32("title"));
        let field_id_title = schema.get_field("title").unwrap();
        let config = IndexConfigBuilder::default().build();
        let mut index = Index::new(schema, config).unwrap();
        let mut writer1 = index.writer().unwrap();

        let mut d = Document::new();
        d.add_text(field_id_title.clone(), "bb");
        writer1.add(&d).unwrap();

        let mut d1 = Document::new();
        d1.add_text(field_id_title.clone(), "aa");
        writer1.add(&d1).unwrap();

        let reader = index.reader().unwrap();
        let p = reader
            .search(Term::from_field_text(field_id_title, "aa"))
            .unwrap();

        for doc_freq in p.iter() {
            let doc = reader.doc(doc_freq.doc()).unwrap();
            println!("docid:{},doc{:?}", doc_freq.doc(), doc);
        }
    }

    #[test]
    fn test_search() {
        let mut schema = Schema::new();
        schema.add_field(FieldEntry::str("body"));
        schema.add_field(FieldEntry::i32("title"));
        let field_id_title = schema.get_field("title").unwrap();
        println!("field_id_title:{:?}", field_id_title.clone());
        let config = IndexConfigBuilder::default().build();
        let mut index = Index::new(schema, config).unwrap();
        let mut writer1 = index.writer().unwrap();
        let t1 = thread::spawn(move || loop {
            let mut d = Document::new();
            d.add_i32(field_id_title.clone(), 2);
            writer1.add(&d).unwrap();
            break;
        });
        let mut writer2 = index.writer().unwrap();
        let t2 = thread::spawn(move || loop {
            let mut d = Document::new();
            d.add_i32(field_id_title.clone(), 2);
            writer2.add(&d).unwrap();
            break;
        });

        t1.join();
        t2.join();
        // let mut writer3 = index.writer().unwrap();
        // writer3.commit();
        let reader = index.reader().unwrap();
        let p = reader
            .search(Term::from_field_i32(field_id_title, 2))
            .unwrap();

        for x in p.iter() {
            println!("{:?}", x);
        }
    }

    #[test]
    fn test_fieldcache1() {
        let buffer_pool = Arc::new(RingBuffer::new());
        let mut field = FieldCache::new(Arc::downgrade(&buffer_pool));
        let mut doc_id: DocID = 0;
        let valuea = Value::Str("aa");

        doc_id = 2;
        field.add(doc_id, &valuea).unwrap();

        doc_id = 4;
        field.add(doc_id, &valuea).unwrap();
        field.add(doc_id, &valuea).unwrap();
        doc_id = 5;
        field.add(doc_id, &valuea).unwrap();
        field.add(doc_id, &valuea).unwrap();
        field.add(doc_id, &valuea).unwrap();
        doc_id = 8;
        field.add(doc_id, &valuea).unwrap();
        field.commit().unwrap();

        let field_reader = FieldReader::new(field.indexs.clone(), field.share_bytes_block.clone());

        println!("search aa");
        let mut p = field_reader.get("aa".as_bytes()).unwrap();
        for x in p.iter() {
            println!("{:?}", x);
        }
    }

    #[test]
    fn test_fieldcache2() {
        let buffer_pool = Arc::new(RingBuffer::new());
        let mut field = FieldCache::new(Arc::downgrade(&buffer_pool));
        let mut doc_id: DocID = 0;
        let valuea = Value::Str("aa");
        let valueb = Value::Str("bb");
        let valuec = Value::Str("cc");
        let valued = Value::Str("dd");
        let value1 = Value::I32(1);
        let value2 = Value::I32(2);
        // field.add(doc_id, &valuea).unwrap(); // aa
        field.add(doc_id, &valueb).unwrap(); //
        field.add(doc_id, &value1).unwrap();
        // field.add(doc_id, &value3).unwrap();
        // field.add(doc_id, &value4).unwrap();

        doc_id = 1;
        field.add(doc_id, &valuea).unwrap();
        field.add(doc_id, &valuec).unwrap();

        doc_id = 2;
        field.add(doc_id, &valuea).unwrap();
        field.add(doc_id, &valueb).unwrap();
        field.add(doc_id, &valuec).unwrap();
        field.add(doc_id, &valued).unwrap();
        field.add(doc_id, &value1).unwrap();
        field.add(doc_id, &value2).unwrap();

        doc_id = 3;
        field.add(doc_id, &valuea).unwrap();
        field.add(doc_id, &valuec).unwrap();
        field.add(doc_id, &valued).unwrap();

        doc_id = 4;
        field.add(doc_id, &valuea).unwrap();
        field.add(doc_id, &valued).unwrap();
        field.add(doc_id, &value1).unwrap();
        field.add(doc_id, &value2).unwrap();

        doc_id = 5;
        field.add(doc_id, &valuec).unwrap();
        field.add(doc_id, &valued).unwrap();
        field.commit().unwrap();

        let field_reader = FieldReader::new(field.indexs.clone(), field.share_bytes_block.clone());

        println!("search aa");
        let mut p = field_reader.get("aa".as_bytes()).unwrap();
        for x in p.iter() {
            println!("{:?}", x);
        }

        println!("search bb");
        p = field_reader.get("bb".as_bytes()).unwrap();
        for x in p.iter() {
            println!("{:?}", x);
        }

        println!("search cc");
        p = field_reader.get("cc".as_bytes()).unwrap();
        for x in p.iter() {
            println!("{:?}", x);
        }

        println!("search dd");
        p = field_reader.get("dd".as_bytes()).unwrap();
        for x in p.iter() {
            println!("{:?}", x);
        }

        let mut v = vec![0u8; 0];

        1.serialize(&mut v);
        println!("search 1");
        p = field_reader.get(&v).unwrap();
        for x in p.iter() {
            println!("{:?}", x);
        }
    }
}
