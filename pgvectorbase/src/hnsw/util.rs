use crate::datatype::Vector;
use crate::hnsw;
use crate::hnsw::option::HnswCandidate;
use crate::hnsw::option::HnswElement;
use crate::hnsw::option::HnswElementTuple;
use pgrx::pg_sys;
use pgrx::pg_sys::lappend;
use pgrx::pg_sys::palloc;
use pgrx::pg_sys::BufferGetPage;
use pgrx::pg_sys::Datum;
use pgrx::pg_sys::DatumGetFloat8;
use pgrx::pg_sys::FmgrInfo;
use pgrx::pg_sys::FunctionCall2Coll;
use pgrx::pg_sys::ItemPointer;
use pgrx::pg_sys::ItemPointerCopy;
use pgrx::pg_sys::ItemPointerData;
use pgrx::pg_sys::ItemPointerGetBlockNumber;
use pgrx::pg_sys::ItemPointerGetOffsetNumber;
use pgrx::pg_sys::ItemPointerIsValid;
use pgrx::pg_sys::LockBuffer;
use pgrx::pg_sys::Oid;
use pgrx::pg_sys::PageGetItem;
use pgrx::pg_sys::PageGetItemId;
use pgrx::pg_sys::PointerGetDatum;
use pgrx::pg_sys::ReadBuffer;
use pgrx::pg_sys::Relation;
use pgrx::pg_sys::UnlockReleaseBuffer;
use pgrx::pg_sys::BUFFER_LOCK_SHARE;
use std::ptr;

const HNSW_ELEMENT_TUPLE_TYPE: u8 = 1;
const HNSW_HEAPTIDS: usize = 10;

/// 检查 HnswElementTuple 类型的宏
macro_rules! HnswIsElementTuple {
    ($tup:expr) => {
        // 假设 tup 是一个具有 'type' 字段的结构体实例或指针
        // 如果 $tup 是指针，需要使用 unsafe 块并解引用
        unsafe { (*$tup).type_ == HNSW_ELEMENT_TUPLE_TYPE }
    };
}

pub(crate) unsafe fn get_candidate_distance(
    hc: *mut HnswCandidate,
    q: Datum,
    procinfo: *mut FmgrInfo,
    collation: Oid,
) -> f32 {
    DatumGetFloat8(FunctionCall2Coll(
        procinfo,
        collation,
        q,
        PointerGetDatum((*(*hc).element).vec.cast()),
    )) as f32
}

// 函数定义：创建一个基于给定入口点的HNSW搜索候选对象
// entry_point: 搜索的起始元素指针
// q: 查询向量的Datum（PostgreSQL中的通用数据表示）
// index: 关联的索引关系（表），如果为NULL则表示在内存中的图操作
// procinfo: 函数管理器信息，用于调用距离计算函数
// collation: 排序规则（用于文本比较等）
// loadVec: 布尔值，指示是否加载向量数据（若元素来自磁盘则需要）
pub(crate) unsafe fn hnsw_entry_candidate(
    entry_point: HnswElement,
    q: Datum,
    index: pg_sys::Relation,
    procinfo: *mut FmgrInfo,
    collation: Oid,
    load_vec: bool,
) -> *mut HnswCandidate {
    let hc: *mut HnswCandidate = palloc(size_of::<HnswCandidate>()) as *mut HnswCandidate;
    (*hc).element = entry_point;
    // 判断索引关系是否为空（NULL）
    // 如果index为NULL，通常意味着操作是在纯粹的内存中图（in-memory graph）上进行
    // 例如，可能在索引构建阶段，所有元素已在内存中
    if index.is_null() {
        // 直接计算当前候选元素与查询向量q的距离
        // GetCandidateDistance预计是一个直接计算两向量距离的函数
        (*hc).distance = get_candidate_distance(hc, q, procinfo, collation);
    } else {
        // 如果index不为NULL，表示元素数据可能存储在磁盘页中
        // 调用HnswLoadElement函数：该函数会：
        //  1. 从磁盘（通过index关系）加载指定元素的数据（包括向量，如果loadVec为true）
        //  2. 计算该元素向量与查询向量q的距离，并将结果存入hc->distance
        hnsw_load_element(
            (*hc).element,
            &mut (*hc).distance,
            &q as *const Datum as *mut Datum,
            index,
            procinfo,
            collation,
            load_vec,
        );
    }
    hc
}

pub(crate) unsafe fn hnsw_load_element(
    element: HnswElement,
    distance: *mut f32,
    q: *mut Datum,
    index: Relation,
    procinfo: *mut FmgrInfo,
    collation: Oid,
    load_vec: bool,
) {
    /* 读取元素所在的缓冲区 */
    // 根据元素中存储的块号(blkno)，从索引中读取对应的缓冲区
    let buf = ReadBuffer(index, (*element).blkno);
    // 以共享模式锁定缓冲区，允许其他共享锁但不允许独占写锁，确保读取过程中数据不被修改
    LockBuffer(buf, BUFFER_LOCK_SHARE as i32);
    // 获取缓冲区中的页面指针
    let page = BufferGetPage(buf);
    /* 获取元素元组 */
    // 从页面中根据元素存储的偏移量(offno)获取具体的元组
    // PageGetItemId获取元组标识符，PageGetItem根据标识符获取实际的元组数据
    let etup = PageGetItem(page, PageGetItemId(page, (*element).offno)) as HnswElementTuple;

    assert!(HnswIsElementTuple!(etup));

    // 从元组中加载数据到元素结构体
    hnsw_load_element_from_tuple(element, etup, true, load_vec);

    if distance != ptr::null_mut() {
        *distance = DatumGetFloat8(FunctionCall2Coll(
            procinfo,
            collation,
            *q,
            PointerGetDatum(&(*etup).vec as *const Vector as *const _),
        )) as f32;
    }
    UnlockReleaseBuffer(buf);
}

// Heap TID（Heap Tuple IDentifier，堆元组标识符）在 PostgreSQL 中是一个​​用于唯一标识表中一行数据（称为“元组”）物理位置的核心机制​​。
// 你可以把它理解成数据行在数据库文件中的​​详细门牌号​​。
// 为了让你快速理解，我用一个表格总结它的核心组成部分和工作原理：
// ​​块号 (Block Number)​​
// 表示该行数据存储在哪个​​数据页​​中。每个表文件都被分成多个固定大小的页（默认为 8KB），页从 0开始编号
// 书籍的​​页码​​
// ​​偏移号 (Offset Number)​​
// 表示该行数据在指定数据页的​​行指针数组​​中的索引位置。行指针数组像目录一样，每个条目指向页内一个具体的元组
// 该页的​​行号​​或​​条目号​
unsafe fn hnsw_add_heap_tid(element: HnswElement, heaptid: ItemPointer) {
    let copy = palloc(size_of::<ItemPointerData>());
    ItemPointerCopy(heaptid, copy as _);
    (*element).heaptids = lappend((*element).heaptids, copy);
}

// 函数定义：从HNSW元素元组（Tuple）中加载数据到HNSW元素（Element）结构体
// element: 目标元素（输出参数），用于接收加载的数据
// etup: 源元组，包含存储在磁盘上的数据
// loadHeaptids: 布尔标志，指示是否加载堆元组标识符（heap TIDs）
// loadVec: 布尔标志，指示是否加载向量数据
// ​​内存表示与磁盘表示分离​​的策略
unsafe fn hnsw_load_element_from_tuple(
    element: HnswElement,
    etup: HnswElementTuple,
    load_heaptids: bool,
    load_vec: bool,
) {
    (*element).level = (*etup).level;
    (*element).deleted = (*etup).deleted;
    (*element).neighbor_page = ItemPointerGetBlockNumber(&(*etup).neighbortid);
    (*element).neighbor_offno = ItemPointerGetOffsetNumber(&(*etup).neighbortid);
    (*element).heaptids = ptr::null_mut();

    if load_heaptids {
        for i in 0..HNSW_HEAPTIDS {
            if !ItemPointerIsValid(&(*etup).heaptids[i]) {
                break;
            }
            hnsw_add_heap_tid(
                element,
                &(*etup).heaptids[i] as *const ItemPointerData as *mut ItemPointerData,
            );
        }
    }

    if load_vec {
        (*element).vec = palloc(VectorSize!((*etup).vec.len as usize)) as _;
        std::ptr::copy_nonoverlapping(
            &(*etup).vec,
            (*element).vec,
            VectorSize!((*etup).vec.len as usize),
        );
    }
}
