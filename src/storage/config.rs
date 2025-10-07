/// Expect JSON values to have roughly 3–5 fields with mostly small values.
/// For 1M values, this would require 128MB of memory.
pub const DEFAULT_BLOCK_SIZE_BYTES: usize = 128;

/// Default page size used when not specified
pub const DEFAULT_PAGE_SIZE_BYTES: usize = 32 * 1024 * 1024; // 32MB

pub const DEFAULT_REGION_SIZE_BLOCKS: usize = 8_192;

#[derive(Debug, Default)]
pub(crate) struct StorageOptions {
    //文件大小
    pub page_size_bytes: Option<usize>,
    //块大小
    pub block_size_bytes: Option<usize>,
    //8192 blocks
    pub region_size_blocks: Option<usize>,
}

pub(crate) struct StorageConfig {
    pub page_size_bytes: usize,

    pub block_size_bytes: usize,

    pub region_size_blocks: usize,
}

impl TryFrom<StorageOptions> for StorageConfig {
    type Error = &'static str;

    fn try_from(options: StorageOptions) -> Result<Self, Self::Error> {
        let page_size_bytes = options.page_size_bytes.unwrap_or(DEFAULT_PAGE_SIZE_BYTES);
        let block_size_bytes = options.block_size_bytes.unwrap_or(DEFAULT_BLOCK_SIZE_BYTES);
        let region_size_blocks = options
            .region_size_blocks
            .map(|x| x as usize)
            .unwrap_or(DEFAULT_REGION_SIZE_BLOCKS);

        if block_size_bytes == 0 {
            return Err("Block size must be greater than 0");
        }

        if region_size_blocks == 0 {
            return Err("Region size must be greater than 0");
        }

        if page_size_bytes == 0 {
            return Err("Page size must be greater than 0");
        }

        let region_size_bytes = block_size_bytes * region_size_blocks;

        if page_size_bytes < region_size_bytes {
            return Err("Page size must be greater than or equal to (block size * region size)");
        }

        if page_size_bytes % region_size_bytes != 0 {
            return Err("Page size must be a multiple of (block size * region size)");
        }

        Ok(Self {
            page_size_bytes,
            block_size_bytes,
            region_size_blocks,
        })
    }
}
