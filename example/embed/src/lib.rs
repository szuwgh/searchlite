pub mod error;
pub mod image;
use crate::error::GyResult;
use std::io::{BufRead, Seek};

pub trait ImageEmbed {
    fn embed<R: BufRead + Seek>(r: R, image_format: &str) -> GyResult<()>;
}
