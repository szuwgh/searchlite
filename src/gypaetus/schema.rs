// 每一行数据

use super::util::error::{GyError, GyResult};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use varintrs::{Binary, ReadBytesVarExt, WriteBytesVarExt};
pub type DateTime = chrono::DateTime<chrono::Utc>;

pub trait BinarySerialize: Sized {
    /// Serialize
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()>;
    /// Deserialize
    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self>;
}

pub trait VarIntSerialize: Sized {
    /// Serialize
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<usize>;
    /// Deserialize
    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<(Self, usize)>;
}

pub type DocID = u64;

#[derive(Debug)]
pub struct DocFreq(pub(crate) DocID, pub(crate) u32);

impl DocFreq {
    pub(crate) fn doc_id(&self) -> DocID {
        self.0
    }

    pub(crate) fn freq(&self) -> u32 {
        self.1
    }
}

impl BinarySerialize for DocFreq {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        if self.freq() == 1 {
            let doc_code = self.doc_id() << 1 | 1;
            VUInt(doc_code).binary_serialize(writer)?;
            //let addr = block_pool.write_var_u64(posting.doc_freq_addr, doc_code)?;
            //posting.doc_freq_addr = addr;
        } else {
            VUInt(self.doc_id() << 1).binary_serialize(writer)?;
            // let addr = block_pool.write_var_u64(posting.doc_freq_addr, doc_delta << 1)?;
            VUInt(self.freq() as u64).binary_serialize(writer)?;
            //posting.doc_freq_addr = block_pool.write_vu32(addr, posting.freq)?;
        }
        Ok(())
    }
    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let doc_code = VUInt::binary_deserialize(reader)?.0.val();
        let freq = if doc_code & 1 > 0 {
            1
        } else {
            VUInt::binary_deserialize(reader)?.0.val() as u32
        };
        Ok(DocFreq(doc_code, freq))
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Schema {
    pub fields: Vec<FieldEntry>,
    pub fields_map: HashMap<String, FieldID>,
}

impl Schema {
    pub fn new() -> Schema {
        Schema::default()
    }

    pub fn get_field(&self, field_name: &str) -> Option<FieldID> {
        self.fields_map.get(field_name).cloned()
    }

    //添加一个域
    pub fn add_field(&mut self, mut field_entry: FieldEntry) {
        let field_id = FieldID::from_field_id(self.fields.len() as u32);
        let field_name = field_entry.get_name().to_string();
        field_entry.field_id = field_id.clone();
        self.fields.push(field_entry);
        self.fields_map.insert(field_name, field_id);
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FieldEntry {
    name: String,
    field_id: FieldID,
    field_type: FieldType,
}

impl FieldEntry {
    pub(crate) fn str(field_name: &str) -> FieldEntry {
        FieldEntry {
            name: field_name.to_string(),
            field_id: FieldID::default(),
            field_type: FieldType::Str,
        }
    }

    pub(crate) fn i64(field_name: &str) -> FieldEntry {
        FieldEntry {
            name: field_name.to_string(),
            field_id: FieldID::default(),
            field_type: FieldType::I64,
        }
    }

    pub(crate) fn i32(field_name: &str) -> FieldEntry {
        FieldEntry {
            name: field_name.to_string(),
            field_id: FieldID::default(),
            field_type: FieldType::I32,
        }
    }

    pub(crate) fn u64(field_name: &str) -> FieldEntry {
        FieldEntry {
            name: field_name.to_string(),
            field_id: FieldID::default(),
            field_type: FieldType::U64,
        }
    }

    pub(crate) fn get_name(&self) -> &str {
        &self.name
    }

    pub fn get_field_type(&self) -> &FieldType {
        &self.field_type
    }

    pub fn get_field_id(&self) -> &FieldID {
        &self.field_id
    }
}

pub enum VectorType {
    Flat,
    BinFlat,
    IvfFlat,
    BinIvfFlat,
    IvfPQ,
    IvfSQ8,
    IvfSQ8H,
    NSG,
    HNSW,
    RHNSWFlat,
    RHNSWPQ,
    RHNSWSQ,
    IvfHNSW,
    ANNOY,
    NGTPANNG,
    NGTONNG,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum FieldType {
    Str,
    I64,
    I32,
    U64,
    U32,
    F64,
    F32,
    DATE,
    Bytes,
}

pub struct Vector<V> {
    pub v: V,
    pub payload: Document,
}

impl<V> Vector<V> {
    pub fn with(v: V) -> Vector<V> {
        Self {
            v: v,
            payload: Document::new(),
        }
    }

    pub fn into(self) -> V {
        self.v
    }

    pub fn with_fields(v: V, field_values: Vec<FieldValue>) -> Vector<V> {
        Self {
            v: v,
            payload: Document::from(field_values),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Document {
    pub field_values: Vec<FieldValue>,
}

impl BinarySerialize for Document {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        VUInt(self.field_values.len() as u64).binary_serialize(writer)?;
        for field_value in &self.field_values {
            field_value.binary_serialize(writer)?;
        }
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let num_field_values = VUInt::binary_deserialize(reader)?.0.val() as usize;
        let field_values = (0..num_field_values)
            .map(|_| FieldValue::binary_deserialize(reader))
            .collect::<GyResult<Vec<FieldValue>>>()?;
        Ok(Document::from(field_values))
    }
}

impl Document {
    pub fn new() -> Document {
        Self {
            field_values: Vec::new(),
        }
    }

    pub fn size(&self) -> usize {
        varintrs::vint_size!(self.field_values.len()) as usize
            + self.field_values.iter().map(|f| f.size()).sum::<usize>()
    }

    pub fn add_u64(&mut self, field: FieldID, value: u64) {
        self.add_field_value(FieldValue::new(field, Value::U64(value)));
    }

    pub fn add_i32(&mut self, field: FieldID, value: i32) {
        self.add_field_value(FieldValue::new(field, Value::I32(value)));
    }

    pub fn add_i64(&mut self, field: FieldID, value: i64) {
        self.add_field_value(FieldValue::new(field, Value::I64(value)));
    }

    pub fn add_text(&mut self, field: FieldID, value: &str) {
        self.add_field_value(FieldValue::new(field, Value::String(value.to_string())));
    }

    pub fn from(field_values: Vec<FieldValue>) -> Document {
        Self {
            field_values: field_values,
        }
    }

    pub fn add_field_value(&mut self, field: FieldValue) {
        self.field_values.push(field);
    }

    pub fn sort_fieldvalues(&mut self) {
        self.field_values
            .sort_by_key(|field_value| field_value.field_id.0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize, Default)]
pub struct FieldID(pub u32);

impl FieldID {
    pub(crate) fn from_field_id(field_id: u32) -> FieldID {
        FieldID(field_id)
    }

    #[inline]
    pub(crate) fn size(&self) -> usize {
        4
    }
}

impl BinarySerialize for FieldID {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        self.0.binary_serialize(writer)
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<FieldID> {
        u32::binary_deserialize(reader).map(FieldID)
    }
}

//域定义
#[derive(Debug, PartialEq)]
pub struct FieldValue {
    field_id: FieldID,
    pub(crate) value: Value,
}

impl BinarySerialize for FieldValue {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        self.field_id.binary_serialize(writer)?;
        self.value.binary_serialize(writer)
    }
    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<FieldValue> {
        let field_id = FieldID::binary_deserialize(reader)?;
        let value = Value::binary_deserialize(reader)?;
        Ok(FieldValue {
            field_id: field_id,
            value: value,
        })
    }
}

impl FieldValue {
    pub fn new(field_id: FieldID, value: Value) -> Self {
        Self {
            field_id: field_id,
            value: value,
        }
    }

    fn size(&self) -> usize {
        self.field_id.size() + self.value.size()
    }

    pub fn field_id(&self) -> &FieldID {
        &self.field_id
    }

    pub fn value(&self) -> &Value {
        &self.value
    }
}

const STR_ENCODE: u8 = 0;
const I64_ENCODE: u8 = 1;
const U64_ENCODE: u8 = 2;
const I32_ENCODE: u8 = 3;
const U32_ENCODE: u8 = 4;
const F32_ENCODE: u8 = 5;
const F64_ENCODE: u8 = 6;
const DATE_ENCODE: u8 = 7;
const BYTES_ENCODE: u8 = 8;

#[derive(PartialEq, Debug)]
//域 值类型
pub enum Value {
    Str(&'static str),
    String(String),
    I64(i64),
    U64(u64),
    I32(i32),
    U32(u32),
    F64(f64),
    F32(f32),
    Date(DateTime),
    Bytes(Vec<u8>),
}

impl BinarySerialize for Value {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        match &self {
            Value::Str(s) => {
                STR_ENCODE.binary_serialize(writer)?;
                (*s).to_string().binary_serialize(writer)?;
            }
            Value::String(s) => {
                STR_ENCODE.binary_serialize(writer)?;
                s.binary_serialize(writer)?;
            }
            Value::I64(i) => {
                I64_ENCODE.binary_serialize(writer)?;
                i.binary_serialize(writer)?;
            }
            Value::U64(u) => {
                U64_ENCODE.binary_serialize(writer)?;
                u.binary_serialize(writer)?;
            }
            Value::I32(i) => {
                I32_ENCODE.binary_serialize(writer)?;
                i.binary_serialize(writer)?;
            }
            Value::U32(u) => {
                U32_ENCODE.binary_serialize(writer)?;
                u.binary_serialize(writer)?;
            }
            Value::F64(f) => {
                F64_ENCODE.binary_serialize(writer)?;
                f.binary_serialize(writer)?;
            }
            Value::F32(f) => {
                F32_ENCODE.binary_serialize(writer)?;
                f.binary_serialize(writer)?;
            }
            Value::Date(d) => {
                DATE_ENCODE.binary_serialize(writer)?;
                d.timestamp_nanos().binary_serialize(writer)?;
            }
            Value::Bytes(b) => {
                BYTES_ENCODE.binary_serialize(writer)?;
                b.binary_serialize(writer)?;
            }
        }
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        match u8::binary_deserialize(reader)? {
            STR_ENCODE => Ok(Value::String(String::binary_deserialize(reader)?)),
            I64_ENCODE => Ok(Value::I64(i64::binary_deserialize(reader)?)),
            U64_ENCODE => Ok(Value::U64(u64::binary_deserialize(reader)?)),
            I32_ENCODE => Ok(Value::I32(i32::binary_deserialize(reader)?)),
            U32_ENCODE => Ok(Value::U32(u32::binary_deserialize(reader)?)),
            F64_ENCODE => Ok(Value::F64(f64::binary_deserialize(reader)?)),
            F32_ENCODE => Ok(Value::F32(f32::binary_deserialize(reader)?)),
            DATE_ENCODE => Ok(Value::Date(
                Utc.timestamp_nanos(i64::binary_deserialize(reader)?),
            )),
            BYTES_ENCODE => Ok(Value::Bytes(Vec::<u8>::binary_deserialize(reader)?)),
            _ => Err(GyError::ErrInvalidValueType),
        }
    }
}

impl Value {
    pub fn size(&self) -> usize {
        match &self {
            Value::Str(s) => {
                let str_length = varintrs::vint_size!(s.as_bytes().len()) as usize;
                1 + str_length + s.as_bytes().len()
            }
            Value::String(s) => {
                let str_length = varintrs::vint_size!(s.as_bytes().len()) as usize;
                1 + str_length + s.as_bytes().len()
            }
            Value::I64(_) => 9,
            Value::U64(_) => 9,
            Value::I32(_) => 5,
            Value::U32(_) => 5,
            Value::F64(_) => 9,
            Value::F32(_) => 5,
            Value::Date(_) => 9,
            Value::Bytes(b) => {
                let str_length = varintrs::vint_size!(b.len()) as usize;
                1 + str_length + b.len()
            }
            _ => 0,
        }
    }

    pub fn to_vec(&self) -> GyResult<Vec<u8>> {
        match &self {
            Value::Str(s) => Ok((*s).as_bytes().to_vec()),
            Value::String(s) => Ok(s.as_bytes().to_vec()),
            Value::I64(i) => {
                let mut v = vec![0u8; 8];
                i.binary_serialize(&mut v)?;
                Ok(v)
            }

            Value::U64(i) => {
                let mut v = vec![0u8; 8];
                i.binary_serialize(&mut v)?;
                Ok(v)
            }
            Value::I32(i) => {
                let mut v = vec![0u8; 4];
                i.binary_serialize(&mut v)?;
                Ok(v)
            }
            Value::U32(u) => {
                let mut v = vec![0u8; 4];
                u.binary_serialize(&mut v)?;
                Ok(v)
            }
            Value::F64(f) => {
                let mut v = vec![0u8; 8];
                f.binary_serialize(&mut v)?;
                Ok(v)
            }
            Value::F32(f) => {
                let mut v = vec![0u8; 4];
                f.binary_serialize(&mut v)?;
                Ok(v)
            }
            Value::Date(f) => {
                let mut v = vec![0u8; 4];
                f.timestamp_nanos().binary_serialize(&mut v)?;
                Ok(v)
            }
            Value::Bytes(v) => Ok(v.clone()),
            _ => Ok(Vec::new()),
        }
    }
}

impl<T: BinarySerialize> BinarySerialize for &[T] {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        VUInt(self.len() as u64).binary_serialize(writer)?;
        for it in *self {
            it.binary_serialize(writer)?;
        }
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        todo!()
    }
}

impl<T: BinarySerialize> BinarySerialize for Vec<T> {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        VUInt(self.len() as u64).binary_serialize(writer)?;
        for it in self {
            it.binary_serialize(writer)?;
        }
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Vec<T>> {
        let num_items = VUInt::binary_deserialize(reader)?.0.val();
        let mut items: Vec<T> = Vec::with_capacity(num_items as usize);
        for _ in 0..num_items {
            let item = T::binary_deserialize(reader)?;
            items.push(item);
        }
        Ok(items)
    }
}

impl BinarySerialize for String {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        let data: &[u8] = self.as_bytes();
        VUInt(data.len() as u64).binary_serialize(writer)?;
        writer.write_all(data)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<String> {
        let str_len = VUInt::binary_deserialize(reader)?.0.val() as usize;
        let mut result = String::with_capacity(str_len);
        reader.take(str_len as u64).read_to_string(&mut result)?;
        Ok(result)
    }
}

impl BinarySerialize for u8 {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_u8(*self)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let v = reader.read_u8()?;
        Ok(v)
    }
}

impl BinarySerialize for i32 {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_i32::<BigEndian>(*self)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let v = reader.read_i32::<BigEndian>()?;
        Ok(v as i32)
    }
}

impl BinarySerialize for u32 {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_u32::<BigEndian>(*self)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let v = reader.read_u32::<BigEndian>()?;
        Ok(v as u32)
    }
}

impl BinarySerialize for usize {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_u32::<BigEndian>(*self as u32)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let v = reader.read_u32::<BigEndian>()?;
        Ok(v as usize)
    }
}

impl BinarySerialize for i64 {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_i64::<BigEndian>(*self)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let v = reader.read_i64::<BigEndian>()?;
        Ok(v)
    }
}

impl BinarySerialize for u64 {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_u64::<BigEndian>(*self)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let v = reader.read_u64::<BigEndian>()?;
        Ok(v)
    }
}

impl BinarySerialize for f64 {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_f64::<BigEndian>(*self)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let v = reader.read_f64::<BigEndian>()?;
        Ok(v)
    }
}

impl BinarySerialize for f32 {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_f32::<BigEndian>(*self)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let v = reader.read_f32::<BigEndian>()?;
        Ok(v)
    }
}

#[derive(Debug)]
pub struct VUInt(pub u64);

impl VUInt {
    pub(crate) fn val(&self) -> u64 {
        self.0
    }
}

impl VarIntSerialize for VUInt {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<usize> {
        let i = writer.write_vu64::<Binary>(self.0)?;
        Ok(i)
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<(Self, usize)> {
        let (v, i) = reader.read_vu64::<Binary>();
        if i == 0 {
            return Err(GyError::EOF);
        }
        Ok((VUInt(v), i as usize))
    }
}

#[derive(Debug)]
pub struct VInt(pub i64);

impl BinarySerialize for VInt {
    fn binary_serialize<W: Write>(&self, writer: &mut W) -> GyResult<()> {
        writer.write_vi64::<Binary>(self.0)?;
        Ok(())
    }

    fn binary_deserialize<R: Read>(reader: &mut R) -> GyResult<Self> {
        let (v, _) = reader.read_vi64::<Binary>();
        Ok(VInt(v))
    }
}

impl VInt {
    fn val(&self) -> i64 {
        self.0
    }
}

mod tests {
    use std::io::Cursor;

    use crate::gypaetus::{util::fs::to_json_file, Meta};

    use super::*;

    #[test]
    fn test_time() {
        let u = Utc::now();
        let t = u.timestamp();
        println!("t:{}", t);
    }

    #[test]
    fn test_value() {
        let mut bytes: Vec<u8> = Vec::with_capacity(1024);
        let mut cursor = Cursor::new(&mut bytes);
        let value_1 = Value::String("aa".to_string());
        let value_2 = Value::I64(123);
        let value_3 = Value::U64(123456);
        let value_4 = Value::I32(963);
        let value_5 = Value::U32(123789);
        let value_6 = Value::F64(123.456);
        let value_7 = Value::F32(963.852);
        let value_8 = Value::Date(Utc::now());
        let value_9 = Value::Bytes(vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

        value_1.binary_serialize(&mut cursor).unwrap();
        value_2.binary_serialize(&mut cursor).unwrap();
        value_3.binary_serialize(&mut cursor).unwrap();
        value_4.binary_serialize(&mut cursor).unwrap();
        value_5.binary_serialize(&mut cursor).unwrap();
        value_6.binary_serialize(&mut cursor).unwrap();
        value_7.binary_serialize(&mut cursor).unwrap();
        value_8.binary_serialize(&mut cursor).unwrap();
        value_9.binary_serialize(&mut cursor).unwrap();

        let mut cursor = Cursor::new(&bytes);
        let d_value_1 = Value::binary_deserialize(&mut cursor).unwrap();
        let d_value_2 = Value::binary_deserialize(&mut cursor).unwrap();
        let d_value_3 = Value::binary_deserialize(&mut cursor).unwrap();
        let d_value_4 = Value::binary_deserialize(&mut cursor).unwrap();
        let d_value_5 = Value::binary_deserialize(&mut cursor).unwrap();
        let d_value_6 = Value::binary_deserialize(&mut cursor).unwrap();
        let d_value_7 = Value::binary_deserialize(&mut cursor).unwrap();
        let d_value_8 = Value::binary_deserialize(&mut cursor).unwrap();
        let d_value_9 = Value::binary_deserialize(&mut cursor).unwrap();
        assert_eq!(value_1, d_value_1);
        assert_eq!(value_2, d_value_2);
        assert_eq!(value_3, d_value_3);
        assert_eq!(value_4, d_value_4);
        assert_eq!(value_5, d_value_5);
        assert_eq!(value_6, d_value_6);
        assert_eq!(value_7, d_value_7);
        assert_eq!(value_8, d_value_8);
        assert_eq!(value_9, d_value_9);
    }

    #[test]
    fn test_field() {
        let mut bytes: Vec<u8> = Vec::with_capacity(1024);
        let mut cursor = Cursor::new(&mut bytes);
        let field_1 = FieldValue::new(FieldID::from_field_id(1), Value::String("aa".to_string()));
        let field_2 = FieldValue::new(FieldID::from_field_id(2), Value::I64(123));
        let field_3 = FieldValue::new(FieldID::from_field_id(3), Value::U64(123456));
        let field_4 = FieldValue::new(FieldID::from_field_id(4), Value::I32(963));
        let field_5 = FieldValue::new(FieldID::from_field_id(5), Value::U32(123789));
        let field_6 = FieldValue::new(FieldID::from_field_id(6), Value::F64(123.456));
        let field_7 = FieldValue::new(FieldID::from_field_id(7), Value::F32(963.852));
        let field_8 = FieldValue::new(FieldID::from_field_id(8), Value::Date(Utc::now()));
        let field_9 = FieldValue::new(
            FieldID::from_field_id(9),
            Value::Bytes(vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
        );

        field_1.binary_serialize(&mut cursor).unwrap();
        field_2.binary_serialize(&mut cursor).unwrap();
        field_3.binary_serialize(&mut cursor).unwrap();
        field_4.binary_serialize(&mut cursor).unwrap();
        field_5.binary_serialize(&mut cursor).unwrap();
        field_6.binary_serialize(&mut cursor).unwrap();
        field_7.binary_serialize(&mut cursor).unwrap();
        field_8.binary_serialize(&mut cursor).unwrap();
        field_9.binary_serialize(&mut cursor).unwrap();

        let mut cursor = Cursor::new(&bytes);
        let d_field_1 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        let d_field_2 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        let d_field_3 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        let d_field_4 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        let d_field_5 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        let d_field_6 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        let d_field_7 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        let d_field_8 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        let d_field_9 = FieldValue::binary_deserialize(&mut cursor).unwrap();
        assert_eq!(field_1, d_field_1);
        assert_eq!(field_2, d_field_2);
        assert_eq!(field_3, d_field_3);
        assert_eq!(field_4, d_field_4);
        assert_eq!(field_5, d_field_5);
        assert_eq!(field_6, d_field_6);
        assert_eq!(field_7, d_field_7);
        assert_eq!(field_8, d_field_8);
        assert_eq!(field_9, d_field_9);
        // value_str1.binary_serialize(&mut cursor).unwrap();
    }

    #[test]
    fn test_document() {
        let mut bytes: Vec<u8> = Vec::with_capacity(1024);

        let mut cursor = Cursor::new(&mut bytes);
        let field_1 = FieldValue::new(FieldID::from_field_id(1), Value::String("aa".to_string()));
        let field_2 = FieldValue::new(FieldID::from_field_id(2), Value::I64(123));
        let field_3 = FieldValue::new(FieldID::from_field_id(3), Value::U64(123456));
        let field_4 = FieldValue::new(FieldID::from_field_id(4), Value::I32(963));
        let field_5 = FieldValue::new(FieldID::from_field_id(5), Value::U32(123789));
        let field_6 = FieldValue::new(FieldID::from_field_id(6), Value::F64(123.456));
        let field_7 = FieldValue::new(FieldID::from_field_id(7), Value::F32(963.852));
        let field_8 = FieldValue::new(FieldID::from_field_id(8), Value::Date(Utc::now()));
        let field_9 = FieldValue::new(
            FieldID::from_field_id(9),
            Value::Bytes(vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
        );
        let field_10 = FieldValue::new(FieldID::from_field_id(7), Value::F32(963.852));
        let field_values = vec![
            field_1, field_2, field_3, field_4, field_5, field_6, field_7, field_8, field_9,
            field_10,
        ];
        let doc1 = Document::from(field_values);
        doc1.binary_serialize(&mut cursor).unwrap();
        println!("pos:{}", cursor.position());
        drop(cursor);
        let mut cursor1 = Cursor::new(&bytes);
        let d_doc1 = Document::binary_deserialize(&mut cursor1).unwrap();
        assert_eq!(doc1, d_doc1);
        println!("doc size:{}", doc1.size());
    }

    #[test]
    fn test_meta() {
        let mut schema = Schema::new();
        schema.add_field(FieldEntry::str("body"));
        schema.add_field(FieldEntry::i32("title"));
        let meta = Meta::new(schema);

        crate::gypaetus::fs::to_json_file(&meta, "./meta.json").unwrap();

        let meta1: Meta = crate::gypaetus::fs::from_json_file("./meta.json").unwrap();
        println!("{:?}", meta1);
    }
}
