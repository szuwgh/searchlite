use pgrx::error;
use pgrx::ffi::CString;
use pgrx::opname;
use pgrx::pg_extern;
use pgrx::pg_operator;
use pgrx::pg_schema;

use pgrx::pg_sys::Datum;
use pgrx::pg_sys::Oid;
use pgrx::pgrx_sql_entity_graph::metadata::ArgumentError;
use pgrx::pgrx_sql_entity_graph::metadata::Returns;
use pgrx::pgrx_sql_entity_graph::metadata::ReturnsError;
use pgrx::pgrx_sql_entity_graph::metadata::SqlMapping;
use pgrx::pgrx_sql_entity_graph::metadata::SqlTranslatable;
use pgrx::Array;
use pgrx::FromDatum;
use pgrx::IntoDatum;
use serde::Deserialize;
use serde::Serialize;
use std::alloc::Layout;
use std::ffi::CStr;

use std::ops::Deref;
use std::ops::DerefMut;
use std::ptr::NonNull;
pub const HEADER_MAGIC: u16 = 0x4256; // "VB" for "Vector Base"

use crate::error::VBResult;

#[macro_export]
macro_rules! VectorSize {
    ($dim:expr) => {
        // 假设你的 Vector 结构体有一个字段 `x`，它是一个 float 数组或其他结构的起始标记
        // 这里假设 Vector 结构体定义为：pub struct Vector { pub x: f32, ... }
        std::mem::offset_of!(Vector, x) + size_of::<f32>() * $dim
    };
}

#[macro_export]
macro_rules! DatumGetVector {
    ($datum:expr) => {
        // 假设 pg_detost_datum 已通过 FFI 定义或引入
        (pgrx::pg_sys::pg_detoast_datum($datum) as *mut Vector)
    };
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum VectorTypmod {
    Any,
    Dims(u16),
}

impl VectorTypmod {
    pub fn parse_from_str(s: &str) -> Option<Self> {
        use VectorTypmod::*;
        if let Ok(x) = s.parse::<u16>() {
            Some(Dims(x))
        } else {
            None
        }
    }
    pub fn parse_from_i32(x: i32) -> Option<Self> {
        use VectorTypmod::*;
        if x == -1 {
            Some(Any)
        } else if u16::MIN as i32 <= x && x <= u16::MAX as i32 {
            Some(Dims(x as u16))
        } else {
            None
        }
    }
    pub fn into_option_string(self) -> Option<String> {
        use VectorTypmod::*;
        match self {
            Any => None,
            Dims(x) => Some(i32::from(x).to_string()),
        }
    }
    pub fn into_i32(self) -> i32 {
        use VectorTypmod::*;
        match self {
            Any => -1,
            Dims(x) => i32::from(x),
        }
    }
    pub fn dims(self) -> Option<u16> {
        use VectorTypmod::*;
        match self {
            Any => None,
            Dims(dims) => Some(dims),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VectorBorrowed<'a> {
    dims: u32,
    data: &'a [f32],
}

impl<'a> VectorBorrowed<'a> {
    pub unsafe fn new_unchecked(dims: u32, data: &'a [f32]) -> Self {
        VectorBorrowed { dims, data }
    }

    pub fn dims(&self) -> u32 {
        self.dims
    }

    pub fn data(&self) -> &[f32] {
        self.data
    }
}

pgrx::extension_sql!(
    r#"
CREATE TYPE vector (
    INPUT     = vector_in,
    OUTPUT    = vector_out,
    TYPMOD_IN = vector_typmod_in,
    TYPMOD_OUT = vector_typmod_out,
    STORAGE   = EXTENDED,
    INTERNALLENGTH = VARIABLE,
    ALIGNMENT = double
);
"#,
    name = "vector",
    creates = [Type(Vector)],
    requires = [vector_in, vector_out, vector_typmod_in, vector_typmod_out],
);

#[repr(C, align(8))]
pub struct Vector {
    varlena: u32,
    pub len: u16,
    unused: u16,
    pub x: [f32; 0],
}

impl Vector {
    fn len(&self) -> usize {
        self.len as usize
    }

    pub fn data(&self) -> &[f32] {
        debug_assert_eq!(self.varlena & 3, 0);
        unsafe { std::slice::from_raw_parts(self.x.as_ptr(), self.len as usize) }
    }

    fn as_slice(&self) -> &[f32] {
        unsafe { std::slice::from_raw_parts(self.x.as_ptr(), self.len()) }
    }

    fn into_raw(self) -> *mut Vector {
        Box::into_raw(Box::new(self))
    }

    fn varlena(size: usize) -> u32 {
        (size << 2) as u32
    }

    fn layout(len: usize) -> Layout {
        u16::try_from(len).expect("vector is too large.");
        let layout_alpha = Layout::new::<Vector>();
        let layout_beta = Layout::array::<f32>(len).unwrap();
        let layout = layout_alpha.extend(layout_beta).unwrap().0;
        layout.pad_to_align()
    }

    pub fn as_borrowed(&self) -> &[f32] {
        self.data()
    }

    pub fn euclidean(&self, other: &Self) -> f32 {
        assert_eq!(self.as_slice().len(), other.as_slice().len());
        self.as_slice()
            .iter()
            .zip(other.as_slice().iter()) // 将两个切片的元素配对
            .map(|(l, r)| (l - r).powi(2)) // 计算差值的平方
            .sum::<f32>() // 求和
            .sqrt() // 计算平方根
    }
}

pub enum VectorInput<'a> {
    Owned(VectorOutput),
    Borrowed(&'a mut Vector),
}

impl VectorInput<'_> {
    pub unsafe fn new(mut p: NonNull<Vector>) -> Self {
        let q = NonNull::new(pgrx::pg_sys::pg_detoast_datum(p.cast().as_ptr()).cast()).unwrap();
        if p != q {
            VectorInput::Owned(VectorOutput(q))
        } else {
            VectorInput::Borrowed(p.as_mut())
        }
    }

    pub fn data(&self) -> &[f32] {
        self.deref().data()
    }
}

unsafe impl<'a> SqlTranslatable for VectorInput<'a> {
    fn argument_sql() -> Result<SqlMapping, ArgumentError> {
        Ok(SqlMapping::As(String::from("vector")))
    }
    fn return_sql() -> Result<Returns, ReturnsError> {
        Ok(Returns::One(SqlMapping::As(String::from("vector"))))
    }
}

impl<'a> VectorInput<'a> {
    pub fn into_raw(self) -> *mut Vector {
        match &self {
            VectorInput::Owned(x) => {
                let result = x.0.as_ptr();
                std::mem::forget(self);
                result
            }
            VectorInput::Borrowed(x) => todo!(),
        }
    }
}

impl Deref for VectorInput<'_> {
    type Target = Vector;

    fn deref(&self) -> &Self::Target {
        match self {
            VectorInput::Owned(x) => unsafe { x.0.as_ref() },
            VectorInput::Borrowed(x) => x,
        }
    }
}

impl<'a> DerefMut for VectorInput<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            VectorInput::Owned(x) => unsafe { x.0.as_mut() },
            VectorInput::Borrowed(x) => x,
        }
    }
}

impl<'a> Drop for VectorInput<'a> {
    fn drop(&mut self) {
        match self {
            VectorInput::Owned(x) => unsafe {
                pgrx::pg_sys::pfree(x.0.as_ptr() as _);
            },
            VectorInput::Borrowed(x) => {}
        }
    }
}

unsafe impl pgrx::callconv::BoxRet for VectorInput<'_> {
    unsafe fn box_into<'fcx>(
        self,
        fcinfo: &mut pgrx::callconv::FcInfo<'fcx>,
    ) -> pgrx::datum::Datum<'fcx> {
        unsafe { fcinfo.return_raw_datum(Datum::from(self.into_raw() as *mut ())) }
    }
}

unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for VectorInput<'_> {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        unsafe { arg.unbox_arg_using_from_datum().unwrap() }
    }
}

impl<'a> FromDatum for VectorInput<'a> {
    unsafe fn from_polymorphic_datum(datum: Datum, is_null: bool, _typoid: Oid) -> Option<Self> {
        if is_null {
            None
        } else {
            let ptr = NonNull::new(datum.cast_mut_ptr::<Vector>()).unwrap();
            Some(VectorInput::new(ptr))
        }
    }
}

pub struct VectorOutput(NonNull<Vector>);

impl VectorOutput {
    pub fn from_slice(value: &[f32]) -> VectorOutput {
        unsafe {
            assert!(u16::try_from(value.len()).is_ok());
            let layout = Vector::layout(value.len());
            let ptr = pgrx::pg_sys::palloc(layout.size()) as *mut Vector;
            std::ptr::addr_of_mut!((*ptr).varlena).write(Vector::varlena(layout.size()));
            std::ptr::addr_of_mut!((*ptr).len).write(value.len() as u16);
            std::ptr::addr_of_mut!((*ptr).unused).write(HEADER_MAGIC);

            let data_ptr = (*ptr).x.as_mut_ptr();
            std::ptr::copy_nonoverlapping(value.as_ptr(), data_ptr, value.len());
            let data_size = value.len() * std::mem::size_of::<f32>();
            let total_size = layout.size();
            let padding_start = data_ptr.add(value.len()) as *mut u8; // 尾部起始指针
            let padding_size = total_size - data_size - std::mem::size_of::<Vector>()
                + std::mem::size_of::<[f32; 0]>(); // 填充字节数
                                                   // 将尾部填充为0
            if padding_size > 0 {
                std::ptr::write_bytes(padding_start, 0, padding_size);
            }
            VectorOutput(NonNull::new(ptr).unwrap())
        }
    }

    pub fn from_vec(value: Vec<f32>) -> Self {
        Self::from_slice(&value)
    }

    pub fn into_raw(self) -> *mut Vector {
        let result = self.0.as_ptr();
        std::mem::forget(self);
        result
    }
}

unsafe impl pgrx::datum::UnboxDatum for VectorOutput {
    type As<'src> = VectorOutput;
    #[inline]
    unsafe fn unbox<'src>(d: pgrx::datum::Datum<'src>) -> Self::As<'src>
    where
        Self: 'src,
    {
        let p = NonNull::new(d.sans_lifetime().cast_mut_ptr::<Vector>()).unwrap();
        let q = unsafe {
            NonNull::new(pgrx::pg_sys::pg_detoast_datum(p.cast().as_ptr()).cast()).unwrap()
        };
        if p != q {
            VectorOutput(q)
        } else {
            let header = p.as_ptr();
            let vector = unsafe { (*header).as_borrowed() };
            VectorOutput::from_slice(vector)
        }
    }
}

unsafe impl SqlTranslatable for VectorOutput {
    fn argument_sql() -> Result<SqlMapping, ArgumentError> {
        Ok(SqlMapping::As(String::from("vector")))
    }
    fn return_sql() -> Result<Returns, ReturnsError> {
        Ok(Returns::One(SqlMapping::As(String::from("vector"))))
    }
}

unsafe impl pgrx::callconv::BoxRet for VectorOutput {
    unsafe fn box_into<'fcx>(
        self,
        fcinfo: &mut pgrx::callconv::FcInfo<'fcx>,
    ) -> pgrx::datum::Datum<'fcx> {
        unsafe { fcinfo.return_raw_datum(Datum::from(self.into_raw() as *mut ())) }
    }
}

unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for VectorOutput {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        unsafe { arg.unbox_arg_using_from_datum().unwrap() }
    }
}

impl IntoDatum for VectorOutput {
    fn into_datum(self) -> Option<Datum> {
        Some(Datum::from(self.into_raw() as *mut ()))
    }

    fn type_oid() -> Oid {
        pgrx::wrappers::regtypein("vector")
    }
}

impl Deref for VectorOutput {
    type Target = Vector;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for VectorOutput {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl Drop for VectorOutput {
    fn drop(&mut self) {
        unsafe {
            pgrx::pg_sys::pfree(self.0.as_ptr() as _);
        }
    }
}

impl FromDatum for VectorOutput {
    unsafe fn from_polymorphic_datum(datum: Datum, is_null: bool, _typoid: Oid) -> Option<Self> {
        if is_null {
            None
        } else {
            let p = NonNull::new(datum.cast_mut_ptr::<Vector>())?;
            let q =
                unsafe { NonNull::new(pgrx::pg_sys::pg_detoast_datum(p.cast().as_ptr()).cast())? };
            if p != q {
                Some(VectorOutput(q))
            } else {
                let header = p.as_ptr();
                let vector = unsafe { (*header).as_borrowed() };
                Some(VectorOutput::from_slice(vector))
            }
        }
    }
}

#[pg_extern(immutable, parallel_safe, strict)]
fn vector_in(input: &CStr, _oid: Oid, typmod: i32) -> VBResult<VectorOutput> {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum State {
        MatchingLeft,
        Reading,
        MatchedRight,
    }
    use State::*;
    let input = input.to_bytes();
    let typmod = VectorTypmod::parse_from_i32(typmod).ok_or("Invalid type modifier")?;
    let mut vector = Vec::<f32>::with_capacity(typmod.dims().unwrap_or(0) as usize);
    let mut state = MatchingLeft;
    let mut token: Option<String> = None;
    for &c in input {
        match (state, c) {
            (MatchingLeft, b'[') => {
                state = Reading;
            }
            (Reading, b'0'..=b'9' | b'.' | b'e' | b'+' | b'-') => {
                let token = token.get_or_insert(String::new());
                token.push(char::from_u32(c as u32).ok_or("Bad number")?);
            }
            (Reading, b',') => {
                let token = token.take().ok_or("Expect a number.")?;
                vector.push(token.parse::<f32>()?.into());
            }
            (Reading, b']') => {
                if let Some(token) = token.take() {
                    vector.push(token.parse::<f32>()?.into());
                }
                state = MatchedRight;
            }
            (_, b' ') => {}
            _ => return Err(format!("Bad charactor with ascii {:#x}.", c).into()),
        }
    }
    if state != MatchedRight {
        return Err("Bad sequence.".into());
    }
    if let Some(dims) = typmod.dims() {
        if dims as usize != vector.len() {
            return Err("The dimensions are unmatched with the type modifier.".into());
        }
    }
    Ok(VectorOutput::from_vec(vector))
}

#[pg_extern(immutable, parallel_safe, strict)]
fn vector_out(vector: VectorInput<'_>) -> CString {
    let mut buffer = String::new();
    buffer.push('[');
    if let Some(&x) = vector.data().first() {
        buffer.push_str(format!("{}", x).as_str());
    }
    for &x in vector.data().iter().skip(1) {
        buffer.push_str(format!(", {}", x).as_str());
    }
    buffer.push(']');
    CString::new(buffer).unwrap()
}

#[pg_extern(immutable, parallel_safe, strict)]
fn vector_typmod_in(list: Array<&CStr>) -> i32 {
    if list.is_empty() {
        -1
    } else if list.len() == 1 {
        let s = list.get(0).unwrap().unwrap().to_str().unwrap();
        let typmod = VectorTypmod::parse_from_str(s).expect("Invaild typmod.");
        typmod.into_i32()
    } else {
        panic!("Invaild typmod.");
    }
}

#[pg_extern(immutable, parallel_safe, strict)]
fn vector_typmod_out(typmod: i32) -> CString {
    if typmod > 0 {
        CString::new(format!("({})", typmod)).unwrap()
    } else {
        CString::new("(unspecified)").unwrap()
    }
}

#[pgrx::pg_operator(immutable, parallel_safe, requires = ["vector"])]
fn euclidean_distance(a: VectorInput, b: VectorInput) -> f32 {
    // 自定义距离逻辑
    a.euclidean(&b)
}

#[pgrx::pg_operator(immutable, parallel_safe, requires = ["vector"])]
#[opname(<->)]
fn operator_euclidean_distance(a: VectorInput, b: VectorInput) -> f32 {
    euclidean_distance(a, b)
}
