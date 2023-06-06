use std::borrow::BorrowMut;
use std::io::{Read, Write};
use std::sync::RwLock;

use crate::schema::VecID;
use crate::util::error::GyResult;
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::Weak;
use varintrs::{Binary, ReadBytesVarExt, WriteBytesVarExt};
//参考 lucene 设计 缓存管理
//https://www.cnblogs.com/forfuture1978/archive/2010/02/02/1661441.html

//pub(crate) const SIZE_CLASS: [usize; 10] = [9, 18, 24, 34, 44, 64, 84, 104, 148, 204];
pub(crate) const BLOCK_SIZE_CLASS: [usize; 10] = [9, 10, 11, 12, 13, 14, 15, 16, 17, 18];
const LEVEL_CLASS: [usize; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 9];
const BYTE_BLOCK_SIZE: usize = 32; //1 << 15; //64 KB
const POINTER_LEN: usize = 4;
pub(super) type Addr = usize;
pub(super) trait TextIndex {
    fn insert(&mut self, k: Vec<u8>, v: u64);
}

unsafe impl Send for RingBuffer {}
unsafe impl Sync for RingBuffer {}

// 一写多读
pub(super) struct RingBuffer(UnsafeCell<ByteBlockPool>);

impl RingBuffer {
    pub(super) fn new() -> RingBuffer {
        RingBuffer(UnsafeCell::from(ByteBlockPool::new()))
    }

    pub(super) fn borrow(&self) -> &ByteBlockPool {
        unsafe { &*self.0.get() }
    }
    pub(super) fn borrow_mut(&self) -> &mut ByteBlockPool {
        unsafe { &mut *self.0.get() }
    }
    pub(super) fn iter() {}
}

pub(super) struct ByteBlockPool {
    pub(super) buffers: Vec<Box<[u8]>>,
    pos: Addr,
    used_pos: Addr,
    buffer_pos: Addr,
}

impl ByteBlockPool {
    pub(super) fn new() -> ByteBlockPool {
        Self {
            buffers: Vec::with_capacity(128),
            pos: 0,
            used_pos: 0,
            buffer_pos: 0,
        }
    }

    fn write_u8(&mut self, pos: Addr, v: u8) -> GyResult<Addr> {
        self.pos = pos;
        let x = self.write(&[v])?;
        Ok(x)
    }

    pub(super) fn write_array(&mut self, pos: Addr, v: &[u8]) -> Result<Addr, std::io::Error> {
        self.pos = pos;
        self.write(v)?;
        Ok(self.pos)
    }

    pub(super) fn write_vusize(&mut self, pos: Addr, v: usize) -> Result<Addr, std::io::Error> {
        self.pos = pos;
        self.write_vu64::<Binary>(v as u64)?;
        Ok(self.pos)
    }

    pub(super) fn write_vu32(&mut self, pos: Addr, v: u32) -> Result<Addr, std::io::Error> {
        self.pos = pos;
        self.write_vu64::<Binary>(v as u64)?;
        Ok(self.pos)
    }

    // pub(super) fn write_vec_id(&mut self, pos: Addr, v: VecID) -> Result<Addr, std::io::Error> {
    //     self.pos = pos;
    //     self.write_vu64::<Binary>(v as u64)?;
    //     Ok(self.pos)
    // }

    pub(super) fn write_u64(&mut self, pos: Addr, v: u64) -> Result<Addr, std::io::Error> {
        self.pos = pos;
        self.write_vu64::<Binary>(v)?;
        Ok(self.pos)
    }

    fn next_bytes(&mut self, cur_level: Addr, last: Option<PosTuple>) -> Addr {
        let next_level = LEVEL_CLASS[cur_level];
        self.alloc_bytes(next_level, last)
    }

    fn get_bytes(&self, start_addr: Addr, limit: Addr) -> &[u8] {
        let pos_tuple = Self::get_pos(start_addr);
        &self
            .buffers
            .get(pos_tuple.0)
            .expect("buffer block out of bounds")[pos_tuple.1..pos_tuple.1 + limit]
    }

    fn get_bytes_mut(&mut self, start_addr: Addr, limit: usize) -> &mut [u8] {
        let pos_tuple = Self::get_pos(start_addr);
        &mut self
            .buffers
            .get_mut(pos_tuple.0)
            .expect("buffer block out of bounds")[pos_tuple.1..pos_tuple.1 + limit]
    }

    fn get_next_addr(&self, limit: Addr) -> Addr {
        let pos_tuple = Self::get_pos(limit);
        let b = self.buffers.get(pos_tuple.0).unwrap();
        let next_addr = (((b[pos_tuple.1]) as Addr & 0xff) << 24)
            + (((b[pos_tuple.1 + 1]) as Addr & 0xff) << 16)
            + (((b[pos_tuple.1 + 2]) as Addr & 0xff) << 8)
            + ((b[pos_tuple.1 + 3]) as Addr & 0xff);
        next_addr
    }

    pub(super) fn new_bytes(&mut self, size: usize) -> Addr {
        if self.buffers.is_empty() {
            self.expand_buffer();
        }
        if self.buffer_pos + size > BYTE_BLOCK_SIZE {
            self.expand_buffer();
        }
        let buffer_pos = self.buffer_pos;
        self.buffer_pos += size;
        buffer_pos + self.used_pos
    }

    fn alloc_bytes(&mut self, next_level: usize, last: Option<PosTuple>) -> Addr {
        //申请新的内存块
        let new_size = BLOCK_SIZE_CLASS[next_level];
        let pos = self.new_bytes(new_size);
        let buf = self.buffers.last_mut().unwrap();

        buf[self.buffer_pos - POINTER_LEN] = (16 | next_level) as u8; //写入新的内存块边界

        if let Some(last_pos) = last {
            //在上一个内存块中最后四个byte 写入下一个内存块的地址
            let b = self.buffers.get_mut(last_pos.0).unwrap();
            let slice = &mut b[last_pos.1..last_pos.1 + POINTER_LEN];
            for i in 0..slice.len() {
                slice[i] = (pos >> (8 * (3 - i)) as usize) as u8;
            }
        }
        // 返回申请的内存块首地址
        pos
    }

    fn expand_buffer(&mut self) {
        let v = vec![0u8; BYTE_BLOCK_SIZE];
        self.buffers.push(v.into_boxed_slice());
        self.buffer_pos = 0;
        if self.buffers.len() > 1 {
            self.used_pos += BYTE_BLOCK_SIZE;
        }
    }

    fn get_pos(pos: Addr) -> PosTuple {
        let m = pos / BYTE_BLOCK_SIZE;
        let n = pos & (BYTE_BLOCK_SIZE - 1);
        return PosTuple(m, n);
    }
}

struct PosTuple(usize, usize);

impl Write for ByteBlockPool {
    // 在 byteblockpool 写入 [u8]
    // 当内存块不足时将申请新得内存块
    fn write(&mut self, mut x: &[u8]) -> Result<usize, std::io::Error> {
        let total = x.len();
        while x.len() > 0 {
            let mut pos_tuple = Self::get_pos(self.pos);
            let (i, cur_level) = {
                let b = self.buffers.get_mut(pos_tuple.0).unwrap();
                let i = x.iter().enumerate().find(|(i, v)| {
                    if b[pos_tuple.1 + *i] == 0 {
                        // 在buffer数组中写入数据
                        b[pos_tuple.1 + *i] = **v;
                        return false;
                    }
                    true
                });
                if i.is_none() {
                    self.pos += x.len();
                    return Ok(total);
                }
                let i = i.unwrap().0;
                pos_tuple.1 += i;
                let level = b[pos_tuple.1] & 15u8;
                (i, level)
            };
            //申请新的内存块
            self.pos = self.next_bytes(cur_level as usize, Some(pos_tuple));
            x = &x[i..];
        }
        Ok(total)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        Ok(())
    }
}

pub(super) struct RingBufferReader<'a> {
    pool: &'a RingBuffer,
    start_addr: Addr,
    end_addr: Addr,
    limit: usize, //获取每一个块能读取的长度限制
    level: usize,
    eof: bool,
    first: bool,
}

impl<'a> Iterator for RingBufferReader<'a> {
    type Item = BlockData<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }
        match self.next_block() {
            Ok(m) => Some(BlockData {
                data: m,
                limit: self.limit,
            }),
            Err(_) => None,
        }
    }
}

impl<'a> RingBufferReader<'a> {
    fn new(pool: &'a RingBuffer, start_addr: Addr, end_addr: Addr) -> RingBufferReader<'a> {
        let reader = Self {
            pool: pool,
            start_addr: start_addr,
            end_addr: end_addr,
            limit: 0,
            level: 0,
            eof: false,
            first: true,
        };
        reader
    }

    // pub(crate) fn get_first_block(&self) -> Result<BlockData<'a>,std::io::Error>{

    //     let b = self.pool.borrow().get_bytes(self.start_addr, self.limit);
    //     let block = Block{};
    //     Ok()
    // }

    pub(crate) fn next_block(&mut self) -> Result<&'a [u8], std::io::Error> {
        if self.eof {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        if self.first {
            self.limit =
                if self.start_addr + BLOCK_SIZE_CLASS[self.level] - POINTER_LEN >= self.end_addr {
                    self.end_addr - self.start_addr
                } else {
                    BLOCK_SIZE_CLASS[self.level] - POINTER_LEN
                };
            self.first = false;
        } else {
            self.level = LEVEL_CLASS[((16 | self.level) as u8 & 15u8) as usize];
            let next_addr = self
                .pool
                .borrow()
                .get_next_addr(self.start_addr + self.limit);
            self.start_addr = next_addr;
            self.limit =
                if self.start_addr + BLOCK_SIZE_CLASS[self.level] - POINTER_LEN >= self.end_addr {
                    self.end_addr - self.start_addr
                } else {
                    BLOCK_SIZE_CLASS[self.level] - POINTER_LEN
                };
        }
        let b = self.pool.borrow().get_bytes(self.start_addr, self.limit);
        if self.start_addr + self.limit >= self.end_addr {
            self.eof = true;
        }
        Ok(b)
    }
}

// 快照读写
pub struct SnapshotReader<'a> {
    offset: Addr,
    reader: RingBufferReader<'a>,
    cur_block: BlockData<'a>,
}

impl<'a> SnapshotReader<'a> {
    fn new(mut reader: RingBufferReader<'a>) -> GyResult<SnapshotReader<'a>> {
        let block = reader
            .next()
            .ok_or(std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        Ok(Self {
            // start_addr: start_addr,-
            offset: 0,
            reader: reader,
            cur_block: block,
        })
    }
}

impl<'a> Read for SnapshotReader<'a> {
    fn read(&mut self, x: &mut [u8]) -> Result<usize, std::io::Error> {
        for i in 0..x.len() {
            if self.offset == self.cur_block.limit {
                self.cur_block = self
                    .reader
                    .next()
                    .ok_or(std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
                self.offset = 0;
            }
            x[i] = self.cur_block.data[self.offset];
            self.offset += 1;
        }
        Ok(x.len())
    }
}

pub struct BlockData<'a> {
    data: &'a [u8],
    limit: usize,
}

// impl Read for ByteBlockReader<'_> {
//     fn read(&mut self, x: &mut [u8]) -> Result<usize, std::io::Error> {
//         // let pool = self.pool.upgrade().unwrap();
//         let mut pos = self.start_pos;
//         let limit = self.limit;
//         let mut i: usize = 0;
//         while i < x.len() {
//             if pos == limit {
//                 self.next_block(limit)?;
//                 limit = self.limit;
//                 pos = self.start_pos;
//             }
//             x[i] = self.pool.get_u8(pos);
//             pos += 1;
//             i += 1;
//         }
//         self.start_pos = pos;
//         Ok(i)
//     }
// }

#[cfg(test)]
mod tests {
    use super::*;

    const uvar_test: [u32; 19] = [
        0,
        1,
        2,
        10,
        20,
        63,
        64,
        65,
        127,
        128,
        129,
        255,
        256,
        257,
        517,
        768,
        5976,
        59767464,
        1 << 32 - 1,
    ];

    #[test]
    fn test_level() {
        let mut up: u8 = 16 | 0;
        for x in 1..20 {
            let level = up & 15;
            println!("level:{}", level);
            let newLevel = LEVEL_CLASS[level as usize];
            println!("newLevel:{}", newLevel);
            up = (16 | newLevel) as u8;
            println!("up:{}", up);
        }
    }

    #[test]
    fn test_iter() {
        let x = [0, 0, 0, 1, 0, 0];
        let i = x.iter().enumerate().find(|(i, v)| {
            println!("{}", **v);
            if **v != 0 {
                return true;
            }
            false
        });
        println!("{:?}", i);
    }

    #[test]
    fn test_slice() {
        let mut slice = [0, 0, 0, 0];
        for i in 0..slice.len() {
            slice[i] = (256 >> (8 * (3 - i)) as usize) as u8;
        }
        println!("slice{:?}", slice);

        let next_addr = (((slice[0]) as Addr & 0xff) << 24)
            + (((slice[1]) as Addr & 0xff) << 16)
            + (((slice[2]) as Addr & 0xff) << 8)
            + ((slice[3]) as Addr & 0xff);
        println!("next_addr{:?}", next_addr);
    }

    #[test]
    fn test_write() {
        let mut b = RingBuffer::new();
        let x: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let start = b.borrow_mut().alloc_bytes(0, None);
        let mut end = b.borrow_mut().write_array(start, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();
        end = b.borrow_mut().write_array(end, &x).unwrap();

        println!("b:{:?}", b.borrow().buffers);
        println!("start:{},end:{}", start, end);

        let reader = RingBufferReader::new(&b, start, end);
        for v in reader {
            println!("b:{:?},len:{:?}", v.data, v.data.len());
        }
    }

    const u64var_test: [u64; 23] = [
        1,
        2,
        10,
        20,
        63,
        64,
        65,
        127,
        128,
        129,
        255,
        256,
        257,
        517,
        768,
        5976746468,
        88748464645454,
        5789627789625558,
        18446744073709551,
        184467440737095516,
        1844674407370955161,
        18446744073709551615,
        1 << 64 - 1,
    ];

    #[test]
    fn test_read_Int() {
        let mut b = RingBuffer::new();
        let start = b.borrow_mut().alloc_bytes(0, None);
        let mut end = b.borrow_mut().write_u64(start, 0).unwrap();
        for i in u64var_test {
            end = b.borrow_mut().write_u64(end, i).unwrap();
        }

        let reader = RingBufferReader::new(&b, start, end);
        let mut r = SnapshotReader::new(reader).unwrap();
        for _ in 0..u64var_test.len() + 1 {
            let (i, _) = r.read_vu64::<Binary>();
            println!("i:{}", i);
        }
    }

    use std::thread;
    #[test]

    fn test_read() {
        let mut b = RingBuffer::new();
        // let x: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        // let mut pos = b.alloc_bytes(0, None);
        // for v in uvar_test {
        //     pos = b.write_vu32(pos, v).unwrap();
        // }

        let pool = Arc::new(b);
        let pool1 = Arc::clone(&pool);
        let t1 = thread::spawn(move || loop {
            let c = &pool1;
            let mut reader = RingBufferReader::new(c, 0, 0);
            for v in uvar_test {
                // let x = reader.read_vu32();
                //println!("x{:?}", x);
            }
        });
        let pool2 = Arc::clone(&pool);
        let t2 = thread::spawn(move || loop {
            let c = &pool2;
            let mut reader = RingBufferReader::new(c, 0, 0);
            for v in uvar_test {
                // let x = reader.read_vu32();
                //println!("x{:?}", x);
            }
        });
        t1.join();
        t2.join();
    }
}
