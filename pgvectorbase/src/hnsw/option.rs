use crate::datatype::Vector;
use crate::error::VBResult;
use crate::hnsw::build::build_callback;
use core::ffi::c_void;
use pgrx::info;
use pgrx::list::List::Nil;
use pgrx::notice;
use pgrx::pg_sys;
use pgrx::pg_sys::index_getprocinfo;
use pgrx::pg_sys::palloc;
use pgrx::pg_sys::pfree;
use pgrx::pg_sys::pg_detoast_datum;
use pgrx::pg_sys::BlockNumber;
use pgrx::pg_sys::Datum;
use pgrx::pg_sys::FmgrInfo;
use pgrx::pg_sys::ItemPointerCopy;
use pgrx::pg_sys::ItemPointerData;
use pgrx::pg_sys::List;
use pgrx::pg_sys::MemoryContext;
use pgrx::pg_sys::MemoryContextReset;
use pgrx::pg_sys::MemoryContextSwitchTo;
use pgrx::pg_sys::OffsetNumber;
use pgrx::pg_sys::Oid;
use pgrx::PgBox;
use std::ptr;

const HNSW_DISTANCE_PROC: u16 = 1;

const HNSW_MAGIC_NUMBER: u32 = 0xA953A953;
const HNSW_DEFAULT_M: i32 = 16; // 默认 M 参数
const HNSW_MAX_DIM: i32 = 2000; // 最大支持维度
const HNSW_PAGE_ID: u16 = 0xFF90;

macro_rules! HnswGetLayerM {
    ($m:expr, $layer:expr) => {
        if $layer == 0 {
            $m * 2
        } else {
            $m
        }
    };
}

//Optimal ML from paper
macro_rules! HnswGetMl {
    ($m:expr) => {
        1.0 / ($m as f64).ln()
    };
}

/// 安全地获取 Vector（假设 pg_detoast_datum 和 Vector 已正确定义）
fn datum_get_vector(datum: Datum) -> Option<*mut Vector> {
    let ptr = unsafe { pg_detoast_datum(datum.cast_mut_ptr()) };
    if ptr.is_null() {
        None
    } else {
        // 将解压后的指针转换为 Vector 的可变引用
        // 使用 'static 生命周期需极度谨慎，实际应根据内存上下文确定正确生命周期
        Some(unsafe { ptr as *mut Vector })
    }
}

macro_rules! DatumGetVector {
    ($x:expr) => {
        // 调用假设存在的解压函数，并将结果转换为 Vector 指针
        // 这是一个不安全操作
        unsafe { &mut *(pg_detoast_datum($x) as *mut Vector) }
    };
}
/// HNSW 索引选项结构体（与 C 端完全兼容）
#[repr(C)]
struct HnswOptions {
    vl_len_: i32,         // varlena 头部 (必须作为第一个字段)
    m: i32,               // 最大连接数
    ef_construction: i32, // 构建时的动态候选列表大小
}

pub(crate) struct HnswBuildState {
    heap: pg_sys::Relation,
    index: pg_sys::Relation,
    index_info: *mut pg_sys::IndexInfo,
    fork_num: pg_sys::ForkNumber::Type, //用于标识表或索引的不同物理文件分支 MAIN_FORKNUM 主数据文件分支，存储实际数据

    // HNSW 参数
    pub(crate) m: i32,
    pub(crate) ef_construction: i32,
    pub(crate) dimensions: i32,

    // 统计信息
    pub(crate) reltuples: f64,
    pub(crate) indtuples: f64,

    // 图结构
    elements: *mut List,
    pub(crate) entry_point: HnswElement,
    pub(crate) ml: f64,
    pub(crate) max_level: i32,
    pub(crate) max_in_memory_elements: f64,
    pub(crate) flushed: bool,
    normvec: *mut Vector,

    // 支持函数
    pub(crate) procinfo: *mut FmgrInfo,
    pub(crate) normprocinfo: *mut FmgrInfo,
    pub(crate) collation: pg_sys::Oid,

    // 内存上下文
    pub(crate) tmp_ctx: MemoryContext,
}

/// HNSW 元页面数据结构
/// 元数据页 (HNSW_METAPAGE_BLKNO = 0)​
#[repr(C)]
pub struct HnswMetaPageData {
    pub magic_number: u32,         // 魔数标识
    pub version: u32,              // 版本号
    pub dimensions: u32,           // 向量维度
    pub m: u16,                    // 最大连接数
    pub ef_construction: u16,      // 构建时候选集大小
    pub entry_blkno: BlockNumber,  // 入口点块号
    pub entry_offno: OffsetNumber, // 入口点偏移
    pub entry_level: i16,          // 入口点层级
    pub insert_page: BlockNumber,  // 插入页面
}

/// HNSW 页面特殊数据
#[repr(C)]
pub struct HnswPageOpaqueData {
    pub nextblkno: pg_sys::BlockNumber, // PostgreSQL 块号类型
    pub unused: u16,                    // 未使用字段
    pub page_id: u16,                   // 页面标识符
}

impl HnswBuildState {
    pub(crate) fn new() -> Self {
        Self {
            heap: ptr::null_mut(),
            index: ptr::null_mut(),
            index_info: ptr::null_mut(),
            fork_num: pg_sys::ForkNumber::MAIN_FORKNUM, // 初始化为主分支
            m: 0,
            ef_construction: 0,
            dimensions: 0,
            reltuples: 0.0,
            indtuples: 0.0,
            elements: ptr::null_mut(),
            entry_point: ptr::null_mut(),
            ml: 0.0,
            max_level: 0,
            max_in_memory_elements: 0.0,
            flushed: false,
            normvec: ptr::null_mut(),
            procinfo: ptr::null_mut(),
            normprocinfo: ptr::null_mut(),
            collation: pg_sys::InvalidOid,
            tmp_ctx: std::ptr::null_mut(),
        }
    }

    pub(crate) unsafe fn init(
        &mut self,
        heap: pg_sys::Relation,
        index: pg_sys::Relation,
        index_info: *mut pg_sys::IndexInfo,
        fork_num: pg_sys::ForkNumber::Type,
    ) -> VBResult<()> {
        // 保存基础关系
        self.heap = heap;
        self.index = index;
        self.index_info = index_info;
        self.fork_num = fork_num;

        // 获取索引参数
        self.m = hnsw_get_M(index);
        self.ef_construction = hnsw_get_ef_construction(index);

        // 获取向量维度
        self.dimensions = hnsw_get_dim(index);
        // 验证参数
        if self.dimensions < 0 {
            return Err("column does not have dimensions".into());
        }

        if self.dimensions > HNSW_MAX_DIM {
            return Err(format!(
                "column cannot have more than {} dimensions for hnsw index",
                HNSW_MAX_DIM,
            )
            .into());
        }
        if self.ef_construction < 2 * self.m {
            return Err("ef_construction must be greater than or equal to 2 * m".into());
        }
        self.reltuples = 0.0;
        self.indtuples = 0.0;

        //获取距离函数
        self.procinfo = index_getprocinfo(index, 1, HNSW_DISTANCE_PROC);
        self.collation = *(*index).rd_indcollation.add(0);

        self.elements = ptr::null_mut();
        self.entry_point = ptr::null_mut();

        self.ml = HnswGetMl!(self.m);

        // 7. 创建临时内存上下文
        unsafe {
            self.tmp_ctx = pg_sys::AllocSetContextCreateInternal(
                pg_sys::CurrentMemoryContext,
                "Hnsw build temporary context".as_ptr() as *const _,
                pg_sys::ALLOCSET_DEFAULT_MINSIZE as usize,
                pg_sys::ALLOCSET_DEFAULT_INITSIZE as usize,
                pg_sys::ALLOCSET_DEFAULT_MAXSIZE as usize,
            );
        }

        Ok(())
    }

    pub(crate) unsafe fn build_index(
        &mut self,
        heap_rel: pg_sys::Relation,
        index_rel: pg_sys::Relation,
        index_info: *mut pg_sys::IndexInfo,
        fork_num: pg_sys::ForkNumber::Type,
    ) -> VBResult<()> {
        self.init(heap_rel, index_rel, index_info, fork_num)?;
        if !self.heap.is_null() {
            self.build_graph(fork_num);
        }
        if !self.flushed {
            self.flush_page()?;
        }
        self.free()?;
        Ok(())
    }

    pub(crate) fn build_graph(&mut self, fork_num: pg_sys::ForkNumber::Type) {
        unsafe {
            // 更新进度条（伪装调用，实际可能需要 pg_stat API）
            // 更新 progress 参数
            // pg_sys::pgstat_progress_incr_param(PROGRESS_PHASE_INDEX, PROGRESS_INCREMENT);

            // 调用不同的扫描函数，视 Postgres 版本而定
            // table_index_build_scan()（在较老版本中为 IndexBuildHeapScan()），
            // 以遍历基础表中的每一行（tuple）
            // 并为其调用一个回调函数即 BuildCallback。这一步的作用是：
            // 对每条表中记录进行处理，从中提取键值（在 HNSW 场景中通常是向量数据）；
            // 将键值插入索引结构，在本例中就是构建 HNSW 图索引；
            // 更新索引构建状态，例如记录已处理的元组数等统计信息
            {
                self.reltuples = pg_sys::table_index_build_scan(
                    self.heap,
                    self.index,
                    self.index_info,
                    true as _,            // allow_sync_scan
                    true as _,            // progress
                    Some(build_callback), // 回调函数指针
                    self as *mut Self as *mut c_void,
                    std::ptr::null_mut(),
                );
            }
        }
    }

    pub(crate) unsafe fn free(&mut self) -> VBResult<()> {
        pfree(self.normvec.cast());
        // 释放内存上下文
        if !self.tmp_ctx.is_null() {
            pgrx::pg_sys::MemoryContextDelete(self.tmp_ctx);

            self.tmp_ctx = std::ptr::null_mut();
        }
        // 清理元素列表
        Ok(())
    }

    // PostgreSQL 页面布局：
    // +-------------------+ <-- page (0x1000)
    // | PageHeaderData    | 24字节
    // +-------------------+ <-- metap (0x1018)
    // | HnswMetaPageData  | 40字节
    // +-------------------+ <-- metap_end (0x1040) pd_lower
    // | 空闲空间          |
    // |                   |
    // | ...               |
    // |                   |
    // +-------------------+ <-- pd_upper
    // | 未使用空间        |
    // +-------------------+
    /// 创建索引元数据页
    pub(crate) fn create_meta_page(&self) -> VBResult<()> {
        unsafe {
            let index: pg_sys::Relation = self.index;
            let fork_num = self.fork_num;
            let buf: pg_sys::Buffer = hnsw_new_buffer(index, fork_num);
            let (state, page): (*mut pg_sys::GenericXLogState, pg_sys::Page) =
                hnsw_init_register_page(index, buf);
            let metap = hnsw_page_get_meta(page);
            (*metap).magic_number = HNSW_MAGIC_NUMBER; // HNSW 魔数
            (*metap).version = 1; // 版本号
            (*metap).dimensions = self.dimensions as u32;
            (*metap).m = self.m as u16;
            (*metap).ef_construction = self.ef_construction as u16;
            (*metap).entry_blkno = pg_sys::InvalidBlockNumber;
            (*metap).entry_offno = pg_sys::InvalidOffsetNumber;
            (*metap).entry_level = -1; // 初始入口点层级为 -1
            (*metap).insert_page = pg_sys::InvalidBlockNumber;
            //设置页面头部的 pd_lower 指向页面内空闲空间的起始位置
            let page_header = page as *mut pg_sys::PageHeaderData;
            let metap_end = (metap as *mut u8).add(std::mem::size_of::<HnswMetaPageData>());
            let page_start = page as *mut u8;
            let pd_lower = metap_end.offset_from(page_start) as usize;
            (*page_header).pd_lower = pd_lower as u16;
            hnsw_commit_buffer(buf, state)
        }
    }

    pub(crate) fn flush_page(&mut self) -> VBResult<()> {
        self.create_meta_page()?;
        self.flushed = true;
        Ok(())
    }
}

// Rust 函数:
unsafe fn hnsw_page_get_meta(page: pg_sys::Page) -> *mut HnswMetaPageData {
    pg_sys::PageGetContents(page) as *mut HnswMetaPageData
}

pub unsafe fn hnsw_commit_buffer(
    buf: pg_sys::Buffer,
    state: *mut pg_sys::GenericXLogState,
) -> VBResult<()> {
    pg_sys::MarkBufferDirty(buf);
    pg_sys::GenericXLogFinish(state);
    pg_sys::UnlockReleaseBuffer(buf);
    Ok(())
}

unsafe fn hnsw_new_buffer(
    index: pg_sys::Relation,
    fork_num: pg_sys::ForkNumber::Type,
) -> pg_sys::Buffer {
    // 使用 pgrx 的缓冲区分配函数
    let buf: pg_sys::Buffer = pg_sys::ReadBufferExtended(
        index,
        fork_num,
        pg_sys::InvalidBlockNumber,
        pg_sys::ReadBufferMode::RBM_NORMAL,
        ptr::null_mut(),
    );
    // 锁定缓冲区
    pg_sys::LockBuffer(buf, pg_sys::BUFFER_LOCK_EXCLUSIVE as i32);
    buf
}

/// 初始化并注册页面 (替代 HnswInitRegisterPage)
/// # 安全
/// 调用者必须确保所有指针有效
unsafe fn hnsw_init_register_page(
    index: pg_sys::Relation,
    buf: pg_sys::Buffer,
) -> (*mut pg_sys::GenericXLogState, pg_sys::Page) {
    // 1. 开始通用 XLog
    let state = pg_sys::GenericXLogStart(index);

    // 2. 注册缓冲区
    let page = pg_sys::GenericXLogRegisterBuffer(state, buf, pg_sys::GENERIC_XLOG_FULL_IMAGE as i32)
        as pg_sys::Page;
    // 3. 初始化页面
    hnsw_init_page(buf, page);
    (state, page)
}

unsafe fn hnsw_init_page(buf: pg_sys::Buffer, page: pg_sys::Page) {
    // 1. 获取页面大小
    let page_size = pg_sys::BufferGetPageSize(buf);
    // 2. 初始化页面结构
    pg_sys::PageInit(page, page_size, std::mem::size_of::<HnswPageOpaqueData>());
    // 3. 设置特殊数据
    let opaque = pg_sys::PageGetSpecialPointer(page) as *mut HnswPageOpaqueData;
    (*opaque).nextblkno = pg_sys::InvalidBlockNumber;
    (*opaque).page_id = HNSW_PAGE_ID;
}

pub fn hnsw_get_dim(index_rel: pg_sys::Relation) -> i32 {
    unsafe {
        let relation = &*index_rel;
        let tupdesc = relation.rd_att;
        let first_attr = &*tupdesc;
        let dimensions = first_attr.attrs.as_slice(1)[0].atttypmod;
        dimensions
    }
}

pub fn hnsw_get_M(index_rel: pg_sys::Relation) -> i32 {
    unsafe {
        let relation = &*index_rel;
        // 获取选项指针
        let opts_ptr = relation.rd_options as *mut HnswOptions;
        if opts_ptr.is_null() {
            return HNSW_DEFAULT_M;
        }
        let opts = &*opts_ptr;
        let m = opts.m;
        m
    }
}

pub fn hnsw_get_ef_construction(index_rel: pg_sys::Relation) -> i32 {
    unsafe {
        let relation = &*index_rel;
        // 获取选项指针
        let opts_ptr = relation.rd_options as *mut HnswOptions;
        if opts_ptr.is_null() {
            return HNSW_DEFAULT_M * 2;
        }
        let opts = &*opts_ptr;
        opts.ef_construction
    }
}

pub type HnswElement = *mut HnswElementData;

// 保证与 C 兼容的内存布局
// 经典的数据库设计思想：​​存储格式（on-disk format）​​
// 与 ​​内存格式（in-memory format）​​ 的分离
// 选择性地从元组中解码并加载向量数据（vec）到 HnswElement。​
// ​这一步是关键​​，如果查询只需要计算距离而不需要返回原始行，
// 甚至可以跳过加载 heaptids，节省了I/O和内存
#[repr(C)]
pub struct HnswElementData {
    // 堆元组 ID 列表（使用 PostgreSQL 的 List 结构）
    pub heaptids: *mut List,

    // 元素所在层级 (0 表示底层)
    pub level: u8,

    // 删除标记
    pub deleted: u8,

    // 各层的邻居数组
    pub neighbors: *mut HnswNeighborArray,

    // 磁盘位置：块号
    pub blkno: pg_sys::BlockNumber,

    // 磁盘位置：偏移号
    pub offno: pg_sys::OffsetNumber,

    // 邻居元组的偏移号
    pub neighbor_offno: pg_sys::OffsetNumber,

    // 邻居元组所在块号
    pub neighbor_page: pg_sys::BlockNumber,

    // 向量数据
    pub vec: *mut Vector,
}

impl HnswElementData {
    /*
     * Add a heap TID to an element
     */
    unsafe fn add_heap_tid(&mut self, heaptid: *const pg_sys::ItemPointerData) {
        // 分配内存并复制 ItemPointerData
        let copy = PgBox::<HnswNeighborArray>::alloc().as_ptr(); //palloc(size_of::<HnswNeighborArray>()); //Box::into_raw(Box::new(unsafe { *heaptid }));
        ItemPointerCopy(heaptid, copy as *mut pg_sys::ItemPointerData);
        self.heaptids = pg_sys::lappend(self.heaptids, copy as *mut c_void);
    }

    unsafe fn init_neighbors(&mut self, m: i32) {
        let level = self.level;
        // 计算需要分配的总内存大小
        let ptr =
            palloc(size_of::<HnswNeighborArray>() * (level + 1) as usize) as *mut HnswNeighborArray;
        for lc in 0..=level {
            let lm = HnswGetLayerM!(m, lc) as usize;
            let neighbor_array = &mut *ptr.add(lc as usize);
            neighbor_array.length = 0;
            neighbor_array.items = palloc(size_of::<HnswCandidate>() * lm) as *mut HnswCandidate;
        }
        self.neighbors = ptr;
    }
}

/// 返回一个 [0.0, 1.0) 区间的随机浮点数
pub fn random_double() -> f64 {
    unsafe { pg_sys::pg_prng_double(&mut pg_sys::pg_global_prng_state) }
}

impl HnswElementData {
    pub(crate) unsafe fn new(
        heaptid: *mut pg_sys::ItemPointerData,
        m: i32,
        ml: f64,
        max_level: i32,
    ) -> HnswElement {
        // 使用 Postgres 的分配器 palloc，Rust 对应为 PgBox::alloc
        let element = palloc(size_of::<HnswElementData>()) as HnswElement;

        // 随机生成级别：-log(RandomDouble()) * ml
        let rand_val = random_double();
        //层数是随机的
        let mut level = (-rand_val.ln() * ml) as i32;

        // 限制 level 不超过 max_level
        if level > max_level {
            level = max_level;
        }

        // 初始化字段
        (*element).heaptids = ptr::null_mut(); // Rust 没有 NIL，使用空指针或自定义
        (*element).add_heap_tid(heaptid);

        (*element).level = level as u8; // 假设 level 是 u8
        (*element).deleted = 0;

        (*element).init_neighbors(m);

        element
    }
}

/// 邻居数组结构
#[repr(C)]
pub struct HnswNeighborArray {
    /// 邻居数量
    pub length: i32,

    /// 邻居候选列表
    pub items: *mut HnswCandidate,
}

/// 邻居候选元素
#[repr(C)]
pub struct HnswCandidate {
    /// 指向的图元素
    pub element: HnswElement,

    /// 与查询向量的距离
    pub distance: f32,
}

// HNSW 邻居元组结构
#[repr(C)]
pub struct HnswNeighborTupleData {
    type_field: u8,                  // 元组类型标识
    unused: u8,                      // 填充字节
    count: u16,                      // 实际存储的邻居数量
    indextids: [ItemPointerData; 0], // 柔性数组（长度由count决定）
}

// 智能指针类型别名
pub type HnswNeighborTuple = *mut HnswNeighborTupleData;

const HNSW_HEAPTIDS: usize = 10;
// 主结构体，与 C 的 HnswElementTupleData 对应
// HnswElementTuple的核心作用是作为 HNSW 索引的​​基本存储单元​​，
// 它将一个向量及其所有必要的元信息（如层级、删除标记、指向原始数据的指针等）
// 紧凑地组织在一起，存储在索引页面中
#[repr(C)]
pub struct HnswElementTupleData {
    pub type_: u8,                                  // 类型标识
    pub level: u8,                                  // 层级信息
    pub deleted: u8,                                // 删除标志 (0/1)
    pub unused: u8,                                 // 填充字节
    pub heaptids: [ItemPointerData; HNSW_HEAPTIDS], // 堆 TID 数组
    pub neighbortid: ItemPointerData,               // 邻居 TID
    pub unused2: u16,                               // 填充
    pub vec: Vector,                                // 向量数据
}

pub type HnswElementTuple = *mut HnswElementTupleData;
