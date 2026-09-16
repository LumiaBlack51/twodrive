use fuser::{FileAttr, FileType};
use std::collections::{HashMap, HashSet};
use std::fs::{self};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use twodrive_core::{FileRecord, MetadataEntry, normalize_cloud_path};

use crate::cache_io::{current_gid, current_uid};
use crate::filesystem::ROOT_INO;

#[derive(Debug, Clone)]
pub(crate) struct InodeTable {
    pub(crate) next_ino: u64,
    pub(crate) path_by_ino: HashMap<u64, String>,
    pub(crate) ino_by_path: HashMap<String, u64>,
    pub(crate) record_by_ino: HashMap<u64, FileRecord>,
    pub(crate) children_by_parent_ino: HashMap<u64, Vec<(u64, FileRecord)>>,
}

impl InodeTable {
    pub(crate) fn new(records: &[FileRecord]) -> Self {
        let mut path_by_ino = HashMap::from([(ROOT_INO, "/".to_string())]);
        let mut ino_by_path = HashMap::from([("/".to_string(), ROOT_INO)]);
        let mut record_by_ino = HashMap::new();
        let mut sorted_records = records.to_vec();
        sorted_records.sort_by(|left, right| left.metadata.path.cmp(&right.metadata.path));

        for (next_ino, record) in (ROOT_INO + 1..).zip(sorted_records) {
            let path = record.metadata.path.clone();
            path_by_ino.insert(next_ino, path.clone());
            ino_by_path.insert(path, next_ino);
            record_by_ino.insert(next_ino, record);
        }

        let mut children_by_parent_ino: HashMap<u64, Vec<(u64, FileRecord)>> = HashMap::new();
        for (child_ino, record) in &record_by_ino {
            let parent_ino = parent_ino_for_path(&ino_by_path, &record.metadata.path);
            children_by_parent_ino
                .entry(parent_ino)
                .or_default()
                .push((*child_ino, record.clone()));
        }
        for children in children_by_parent_ino.values_mut() {
            children.sort_by(|(_, left), (_, right)| {
                right
                    .metadata
                    .is_dir
                    .cmp(&left.metadata.is_dir)
                    .then_with(|| {
                        left.metadata
                            .name
                            .to_lowercase()
                            .cmp(&right.metadata.name.to_lowercase())
                    })
                    .then_with(|| left.metadata.name.cmp(&right.metadata.name))
            });
        }

        Self {
            next_ino: records.len() as u64 + ROOT_INO + 1,
            path_by_ino,
            ino_by_path,
            record_by_ino,
            children_by_parent_ino,
        }
    }

    pub(crate) fn refresh_children(&mut self, parent_path: &str, records: Vec<FileRecord>) {
        let Some(parent_ino) = self.ino_for_path(parent_path) else {
            return;
        };
        let mut children = Vec::with_capacity(records.len());
        for record in records {
            let ino = self.ino_for_path(&record.metadata.path).unwrap_or_else(|| {
                let ino = self.next_ino;
                self.next_ino += 1;
                ino
            });
            self.path_by_ino.insert(ino, record.metadata.path.clone());
            self.ino_by_path.insert(record.metadata.path.clone(), ino);
            self.record_by_ino.insert(ino, record.clone());
            children.push((ino, record));
        }
        let present: HashSet<u64> = children.iter().map(|(ino, _)| *ino).collect();
        if let Some(previous) = self.children_by_parent_ino.get(&parent_ino) {
            for (ino, record) in previous {
                if !present.contains(ino)
                    && self.ino_by_path.get(&record.metadata.path) == Some(ino)
                {
                    self.ino_by_path.remove(&record.metadata.path);
                }
            }
        }
        self.children_by_parent_ino.insert(parent_ino, children);
    }

    pub(crate) fn path_for_ino(&self, ino: u64) -> Option<&str> {
        self.path_by_ino.get(&ino).map(String::as_str)
    }

    pub(crate) fn ino_for_path(&self, path: &str) -> Option<u64> {
        self.ino_by_path.get(&normalize_cloud_path(path)).copied()
    }

    pub(crate) fn ino_for_remote_id(&self, remote_id: &str) -> Option<u64> {
        self.record_by_ino
            .iter()
            .find_map(|(ino, record)| (record.metadata.remote_id == remote_id).then_some(*ino))
    }

    pub(crate) fn record_for_ino(&self, ino: u64) -> Option<&FileRecord> {
        self.record_by_ino.get(&ino)
    }

    pub(crate) fn insert_or_update(&mut self, record: FileRecord) -> u64 {
        if let Some(ino) = self.ino_for_path(&record.metadata.path) {
            self.replace_ino_record(ino, record);
            return ino;
        }

        let ino = self.next_ino;
        self.next_ino += 1;
        self.path_by_ino.insert(ino, record.metadata.path.clone());
        self.ino_by_path.insert(record.metadata.path.clone(), ino);
        self.record_by_ino.insert(ino, record.clone());
        let parent_ino = parent_ino_for_path(&self.ino_by_path, &record.metadata.path);
        self.children_by_parent_ino
            .entry(parent_ino)
            .or_default()
            .push((ino, record));
        ino
    }

    pub(crate) fn replace_ino_record(&mut self, ino: u64, record: FileRecord) {
        let previous_path = self.path_by_ino.insert(ino, record.metadata.path.clone());
        if let Some(previous_path) = previous_path {
            let previous_parent = parent_ino_for_path(&self.ino_by_path, &previous_path);
            if let Some(children) = self.children_by_parent_ino.get_mut(&previous_parent) {
                children.retain(|(child_ino, _)| *child_ino != ino);
            }
            self.ino_by_path.remove(&previous_path);
        }
        self.ino_by_path.insert(record.metadata.path.clone(), ino);
        self.record_by_ino.insert(ino, record.clone());

        let parent_ino = parent_ino_for_path(&self.ino_by_path, &record.metadata.path);
        self.children_by_parent_ino
            .entry(parent_ino)
            .or_default()
            .push((ino, record));
    }

    pub(crate) fn set_size(&mut self, ino: u64, size: u64) {
        if let Some(record) = self.record_by_ino.get_mut(&ino) {
            record.metadata.size = record.metadata.size.max(size);
        }
    }

    pub(crate) fn replace_size(&mut self, ino: u64, size: u64) {
        if let Some(record) = self.record_by_ino.get_mut(&ino) {
            record.metadata.size = size;
        }
    }

    pub(crate) fn remove_ino(&mut self, ino: u64) {
        if let Some(path) = self.path_by_ino.remove(&ino) {
            self.ino_by_path.remove(&path);
        }
        self.record_by_ino.remove(&ino);
        self.children_by_parent_ino.remove(&ino);
        for children in self.children_by_parent_ino.values_mut() {
            children.retain(|(child_ino, _)| *child_ino != ino);
        }
    }

    pub(crate) fn children_for_ino(&self, ino: u64) -> Vec<(u64, FileRecord)> {
        let mut children: Vec<_> = self
            .children_by_parent_ino
            .get(&ino)
            .into_iter()
            .flatten()
            .filter_map(|(ino, _)| {
                self.record_by_ino
                    .get(ino)
                    .cloned()
                    .map(|record| (*ino, record))
            })
            .collect();
        // Sort once when building a directory snapshot, never on every file creation.
        sort_child_records(&mut children);
        children
    }

    pub(crate) fn parent_ino(&self, path: &str) -> Option<u64> {
        let path = normalize_cloud_path(path);
        if path == "/" {
            return Some(ROOT_INO);
        }

        let parent = match path.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(index) => path[..index].to_string(),
        };
        self.ino_for_path(&parent)
    }
}

pub(crate) fn parent_ino_for_path(ino_by_path: &HashMap<String, u64>, path: &str) -> u64 {
    let path = normalize_cloud_path(path);
    if path == "/" {
        return ROOT_INO;
    }

    let parent = match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => path[..index].to_string(),
    };
    ino_by_path.get(&parent).copied().unwrap_or(ROOT_INO)
}

pub(crate) fn attr_for_record(ino: u64, record: &FileRecord) -> FileAttr {
    let kind = file_type(&record.metadata);
    let size = if record.metadata.is_dir {
        0
    } else {
        record.metadata.size
    };
    let time = unix_time(record.metadata.modified_unix);

    FileAttr {
        ino,
        size,
        blocks: size.div_ceil(512),
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind,
        perm: record
            .local_mode
            .unwrap_or(if record.metadata.is_dir { 0o755 } else { 0o644 }),
        nlink: if record.metadata.is_dir { 2 } else { 1 },
        uid: current_uid(),
        gid: current_gid(),
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

pub(crate) fn unlinked_file_attr(ino: u64, file: &fs::File) -> Option<FileAttr> {
    let metadata = file.metadata().ok()?;
    let mut attr = root_attr();
    attr.ino = ino;
    attr.size = metadata.len();
    attr.blocks = attr.size.div_ceil(512);
    attr.kind = FileType::RegularFile;
    attr.perm = 0o644;
    attr.nlink = 0;
    attr.atime = metadata.accessed().unwrap_or(UNIX_EPOCH);
    attr.mtime = metadata.modified().unwrap_or(UNIX_EPOCH);
    attr.ctime = attr.mtime;
    attr.crtime = metadata.created().unwrap_or(UNIX_EPOCH);
    Some(attr)
}

pub(crate) fn root_attr() -> FileAttr {
    let time = SystemTime::now();
    FileAttr {
        ino: ROOT_INO,
        size: 0,
        blocks: 0,
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind: FileType::Directory,
        perm: 0o755,
        nlink: 2,
        uid: current_uid(),
        gid: current_gid(),
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

pub(crate) fn file_type(entry: &MetadataEntry) -> FileType {
    if entry.is_dir {
        FileType::Directory
    } else {
        FileType::RegularFile
    }
}

pub(crate) fn unix_time(seconds: i64) -> SystemTime {
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH
    }
}

pub(crate) fn sort_child_records(children: &mut [(u64, FileRecord)]) {
    children.sort_by(|(_, left), (_, right)| {
        right
            .metadata
            .is_dir
            .cmp(&left.metadata.is_dir)
            .then_with(|| {
                left.metadata
                    .name
                    .to_lowercase()
                    .cmp(&right.metadata.name.to_lowercase())
            })
            .then_with(|| left.metadata.name.cmp(&right.metadata.name))
    });
}
