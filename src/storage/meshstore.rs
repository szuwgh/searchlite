use super::config::StorageConfig;
use super::config::StorageOptions;
use super::page::Page;
use super::tracker::Tracker;
use crate::GyResult;
use parking_lot::RwLock;
use std::path::PathBuf;
const CONFIG_FILENAME: &str = "config.json";
pub struct MeshStore {
    pub(super) config: StorageConfig,
    pub(super) tracker: RwLock<Tracker>,
    pub(super) pages: Vec<Page>,
    base_path: PathBuf,
}

pub type PointOffset = u32;
pub type BlockOffset = u32;
pub type PageId = u32;

impl MeshStore {
    fn new(base_path: PathBuf, options: StorageOptions) -> GyResult<Self> {
        if !base_path.exists() {
            return Err("Base path does not exist".into());
        }
        if !base_path.is_dir() {
            return Err("Base path is not a directory".into());
        }
        let config = StorageConfig::try_from(options)?;
        let config_path = base_path.join(CONFIG_FILENAME);

        let mut storage = Self {
            config: config,
            tracker: RwLock::new(Tracker::new()),
            pages: Vec::new(),
            base_path: base_path,
        };

        let new_page_id = storage.get_next_page_id();

        Ok(storage)
    }

    fn get_next_page_id(&self) -> PageId {
        self.pages.len() as PageId
    }

    pub fn pub_value(&mut self, point_offset: PointOffset) {}
}
