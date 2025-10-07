use super::option::HnswBuildState;
use crate::datatype::Vector;
use crate::error::VBResult;
use crate::hnsw::option::HnswElement;
use crate::hnsw::option::HnswElementData;
use crate::hnsw::util::hnsw_entry_candidate;
use core::ffi::c_void;
use pgrx::info;
use pgrx::notice;
use pgrx::pg_sys;
use pgrx::pg_sys::list_make1_impl;
use pgrx::pg_sys::palloc;
use pgrx::pg_sys::Datum;
use pgrx::pg_sys::FmgrInfo;
use pgrx::pg_sys::ListCell;
use pgrx::pg_sys::MemoryContextReset;
use pgrx::pg_sys::MemoryContextSwitchTo;
use pgrx::pg_sys::NodeTag::T_List;
use pgrx::pg_sys::Oid;
use pgrx::pg_sys::PointerGetDatum;
use std::ptr;
use std::ptr::copy_nonoverlapping;
#[macro_export]
macro_rules! list_make_ptr_cell {
    ($e:expr) => {
        ListCell {
            ptr_value: $e as *mut std::os::raw::c_void,
        }
    };
}

#[macro_export]
macro_rules! list_make1 {
    ($e:expr) => {
        // 假设你的 Vector 结构体有一个字段 `x`，它是一个 float 数组或其他结构的起始标记
        list_make1_impl(T_List, list_make_ptr_cell!($e))
    };
}

// 1. index_rel: pg_sys::Relation
// 这是当前正在构建的索引的关系对象（Relation），用于：
// 调用索引构建相关函数（如插入索引条目）。
// 获取索引的元数据和结构信息。
// 换句话说，它代表正在写入索引的目标关系，以便在回调中执行必要的索引操作。

// 2.heap_tid：
// 这是当前扫描到的主表（heap 表）中元组的物理位置标识符（TID，Tuple ID）。开发者可以使用它来：
// 标记索引条目的对应主表行。
// 在索引中关联该元组，使后续查询能定位原始数据。

// 3. values: *mut pg_sys::Datum
// 这是一个指向 Datum 值数组的裸指针，表示当前元组中用于索引的列的值。
// 每个值以 Datum 类型传递，通常还需处理其可能的格式（如压缩、TOAST）。
// 该参数让回调函数访问 tuple 中的必要数据，用来构造索引条目。

// 4. isnull: *mut bool
// 这是一个与 values 数组一一对应的 Boolean 指针，用于标记每个 values[i] 是否为 NULL。
// 回调处理时可根据 isnull[i] 跳过不需要索引的值，确保数据处理的正确性。

// 5. tuple_is_alive: bool
// 这是一个布尔值，用于指示当前 tuple 是否仍然 alive（存活不删除）。
// 在多版本并发控制 (MVCC) 中，有些版本可能已被标记为无效或删除。
// 尽管此值通常可用于决定是否将该 tuple 包含在索引中，但在构建阶段默认为 true。
// 但开发者仍可用它做进一步的逻辑判断。

// 6. state: *mut c_void
// 这是一个用户自定义的状态指针，在调用时通过 table_index_build_scan(..., callback_state) 传递。
// 它常用于传递构建状态结构体，例如你提到的 HnswBuildState。
// 回调内部的第一步通常是将该 void * 指针转换回 Rust 中对应的类型引用：
pub(crate) unsafe extern "C-unwind" fn build_callback(
    index_rel: pg_sys::Relation,
    heap_tid: pg_sys::ItemPointer,
    values: *mut pg_sys::Datum,
    isnull: *mut bool,
    tuple_is_alive: bool,
    state: *mut c_void,
) {
    let buildstate = &mut *(state as *mut HnswBuildState);

    // 跳过 NULL 值
    if *isnull.offset(0) {
        return;
    }

    // 如果内存中的元素数量超过最大限制，则刷新内存
    if buildstate.indtuples >= buildstate.max_in_memory_elements {
        if !buildstate.flushed {
            notice!(
                "hnsw graph no longer fits into maintenance_work_mem after {} tuples",
                buildstate.indtuples
            );
            info!("Consider increasing maintenance_work_mem to speed up builds.");
            buildstate.flush_page().expect("flush page fail");
        }
        // 切换内存上下文
        let old_ctx = MemoryContextSwitchTo(buildstate.tmp_ctx);

        //插入元组
        // if hnsw_insert_tuple(buildstate.index, values, isnull, heap_tid, buildstate.heap) {
        //     // 更新进度
        //     //update_progress(PROGRESS_CREATEIDX_TUPLES_DONE, buildstate.indtuples + 1);
        //     pg_sys::pgstat_progress_update_param(
        //         pg_sys::PROGRESS_CREATEIDX_TUPLES_DONE as i32,
        //         buildstate.indtuples as i64 + 1,
        //     );
        // }

        // 重置内存上下文
        MemoryContextSwitchTo(old_ctx);
        MemoryContextReset(buildstate.tmp_ctx);

        return;
    }
    let element = HnswElementData::new(heap_tid, buildstate.m, buildstate.ml, buildstate.max_level);
    (*element).vec = palloc(VectorSize!(buildstate.dimensions as usize)) as *mut Vector;
    let old_ctx = MemoryContextSwitchTo(buildstate.tmp_ctx);

    let inserted = insert_tuple(index_rel, values, element, buildstate, ptr::null_mut());
}

/**
 * 在内存中构建索引
 */
unsafe fn insert_tuple(
    index: pg_sys::Relation,
    values: *mut Datum,
    element: HnswElement,
    buildstate: &mut HnswBuildState,
    dup: *mut HnswElement,
) -> VBResult<()> {
    /* 获取距离计算函数信息、排序规则等构建状态参数 */
    let procinfo = buildstate.procinfo;
    let collation = buildstate.collation;
    let entry_point = buildstate.entry_point;
    let ef_construction = buildstate.ef_construction;
    let m = buildstate.m;
    let value_datum = values.add(0);
    // 对向量数据进行解压缩（TOAST是PostgreSQL的存储机制）
    let detoasted_value = PointerGetDatum(pg_sys::pg_detoast_datum(value_datum.cast()).cast());

    if buildstate.normprocinfo != ptr::null_mut() {
        panic!("normprocinfo not supported");
    }
    //将处理后的向量值复制到元素结构中
    copy_nonoverlapping(
        DatumGetVector!(detoasted_value.cast_mut_ptr()),
        (*element).vec,
        VectorSize!(buildstate.dimensions as usize),
    );
    //将元素插入到HNSW图结构中（核心操作）
    hnsw_insert_element(
        element,
        entry_point,
        index,
        procinfo,
        collation,
        m,
        ef_construction,
        false,
    );

    Ok(())
}

/*
 * Algorithm 1 from paper
 */
unsafe fn hnsw_insert_element(
    element: HnswElement,
    entry_point: HnswElement,
    index: pg_sys::Relation,
    procinfo: *mut FmgrInfo,
    collation: Oid,
    m: i32,
    ef_construction: i32,
    existing: bool,
) {
    let level = (*element).level; // 新元素被分配的最高层级
    let q = PointerGetDatum((*element).vec.cast()); // 将新元素的向量转换为Datum格式，便于距离计算
    let skip_element = if existing { element } else { ptr::null() }; // 如果是更新现有元素，则跳过自身
    if entry_point.is_null() {
        return;
    }
    //初始化入口点候选列表，并获取入口点的层级
    let ep = list_make1!(hnsw_entry_candidate(
        entry_point,
        q,
        index,
        procinfo,
        collation,
        true
    ));
    // 第一阶段：从高层到level+1层的贪婪搜索
}
