use super::util::error::{GyError, GyResult};
use super::util::fs::{FileIOSelector, IoSelector, MmapSelector};
use crate::iocopy;
use core::arch::x86_64::*;
use memmap2::{self, Mmap, MmapMut};
use std::fs::{self, File};
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, Weak};

pub(crate) const DEFAULT_WAL_FILE_SIZE: usize = 1 << 20; //512 << 20; //

#[derive(Copy, Clone)]
pub enum IOType {
    FILEIO,
    MMAP,
}

const BLOCK_SIZE: usize = 1 << 15;

unsafe impl Send for Wal {}
unsafe impl Sync for Wal {}

pub(crate) struct WalReader<'a> {
    wal: &'a Wal,
    offset: usize,
}

impl<'a> WalReader<'a> {
    pub(crate) fn from(wal: &'a Wal, offset: usize) -> WalReader<'a> {
        WalReader {
            wal: wal,
            offset: offset,
        }
    }
}

pub(crate) struct Wal {
    io_selector: Box<dyn IoSelector>,
    i: usize,
    j: usize,
    fsize: usize,
    buffer: [u8; BLOCK_SIZE],
}

impl Wal {
    pub(crate) fn new(fname: &Path, fsize: usize, io_type: IOType) -> GyResult<Wal> {
        let io_selector: Box<dyn IoSelector> = match io_type {
            IOType::FILEIO => Box::new(FileIOSelector::new(fname, fsize)?),
            IOType::MMAP => Box::new(MmapSelector::new(fname, fsize)?),
        };
        Ok(Self {
            io_selector: io_selector,
            i: 0,
            j: 0,
            fsize: fsize,
            buffer: [0u8; BLOCK_SIZE],
        })
    }

    pub(crate) fn check_rotate(&self, size: usize) -> GyResult<()> {
        if self.i + size > self.fsize {
            return Err(GyError::ErrWalOverflow);
        }
        Ok(())
    }

    pub(crate) fn write_bytes(&mut self, content: &[u8]) -> GyResult<()> {
        self.write(content)?;
        Ok(())
    }

    pub(crate) fn offset(&self) -> usize {
        self.i
    }
}

impl<'a> Read for WalReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let i = self
            .wal
            .io_selector
            .read(buf, self.offset)
            .map_err(|e| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        self.offset += i;
        Ok(i)
    }
}

impl Write for Wal {
    fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
        let total = buf.len();
        while buf.len() > 0 {
            if self.j >= BLOCK_SIZE {
                self.flush()?;
            }
            let n = iocopy!(&mut self.buffer[self.j..], buf);
            self.j += n;
            buf = &buf[n..];
        }
        Ok(total)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.io_selector
            .write(&self.buffer[..self.j], self.i)
            .map_err(|e| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
        self.io_selector
            .sync()
            .map_err(|e| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
        self.i += self.j;
        self.j = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use std::fs::copy;

    use super::*;

    // fn test_tokenizer() {
    //     let jieba = Jieba::new().unwrap();
    //     //搜索引擎模式
    //     let words = jieba.cut_for_search("小明硕士，毕业于中国科学院计算所，后在日本京都大学深造");

    //     println!("【搜索引擎模式】:{}\n", words.join(" / "));
    // }
    #[test]
    fn test_copy() {
        let mut a = [0; 9];
        let b = [1, 2, 3, 4, 5];
        let i = iocopy!(&mut a, &b);
        println!("{:?},{}", a, i);
    }

    #[test]
    fn test_wal() {
        let mut wal = Wal::new(
            &PathBuf::from("/opt/rsproject/gptgrep/searchlite/00.wal"),
            1 * 1024 * 1024, //512MB
            IOType::MMAP,
        )
        .unwrap();
        let buf = "abcdeee";
        wal.write(buf.as_bytes()).unwrap();
        wal.flush().unwrap();
    }

    use super::super::schema::{BinarySerialize, Document, FieldID, FieldValue, Value};
    use chrono::{TimeZone, Utc};
    #[test]
    fn test_document() {
        let mut wal = Wal::new(
            &PathBuf::from("/opt/rsproject/gptgrep/searchlite/00.wal"),
            1 * 1024 * 1024, //512MB
            IOType::MMAP,
        )
        .unwrap();

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

        let offset = wal.offset();

        let doc1 = Document::from(field_values);
        doc1.binary_serialize(&mut wal).unwrap();
        wal.flush().unwrap();

        let mut wal_read = WalReader::from(&mut wal, offset);
        let doc2 = Document::binary_deserialize(&mut wal_read).unwrap();
        println!("doc2:{:?}", doc2);
        assert_eq!(doc1, doc2);
    }
}
