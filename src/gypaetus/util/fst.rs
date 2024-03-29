use super::error::GyResult;
use furze::{Builder, FST};

pub(crate) fn new() {}

pub(crate) struct FstBuilder {
    fst: Builder<Vec<u8>>,
}

impl FstBuilder {
    pub(crate) fn new() -> FstBuilder {
        FstBuilder {
            fst: Builder::new(Vec::with_capacity(4 * 1024 * 1024)),
        }
    }

    pub(crate) fn add(&mut self, key: &[u8], val: u64) -> GyResult<()> {
        self.fst.add(key, val)?;
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> GyResult<()> {
        self.fst.finish()?;
        Ok(())
    }

    pub(crate) fn get_ref(&self) -> &[u8] {
        self.fst.get()
    }

    pub(crate) fn reset(&mut self) -> GyResult<()> {
        self.fst.reset()?;
        Ok(())
    }
}

pub(crate) struct FstReader<'a> {
    fst: FST<&'a [u8]>,
}

impl<'a> FstReader<'a> {
    pub(crate) fn load(b: &'a [u8]) -> FstReader {
        Self { fst: FST::load(b) }
    }

    pub(crate) fn get(&self, key: &[u8]) -> GyResult<u64> {
        let u = self.fst.get(key)?;
        Ok(u)
    }
}
#[cfg(test)]
mod tests {

    use super::*;
    #[test]
    fn test_fst() {
        let mut fst = FstBuilder::new();
        fst.add(b"aa", 1).unwrap();
        fst.add(b"bb", 2).unwrap();
        fst.finish();
        println!("{:?}", fst.get_ref());
        let fst_r = FstReader::load(fst.get_ref());
        let u = fst_r.get(b"aa").unwrap();
        println!("u:{}", u);
    }
}
