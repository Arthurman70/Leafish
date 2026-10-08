//! Read-only local asset sources. No extraction, downloads, or legacy cache reuse.
use super::Pack;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_RESOURCE: u64 = 64 * 1024 * 1024;
const MAX_INDEX: u64 = 32 * 1024 * 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && !name.contains(':')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

pub struct ArchivePack {
    archive: Mutex<zip::ZipArchive<File>>,
}

impl ArchivePack {
    pub fn open_archive(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|_| invalid("Invalid asset archive"))?;
        if archive.len() > 500_000 {
            return Err(invalid("Too many asset archive entries"));
        }
        let mut names = HashSet::new();
        for index in 0..archive.len() {
            let entry = archive
                .by_index(index)
                .map_err(|_| invalid("Invalid archive entry"))?;
            if !entry.name().starts_with("assets/") || entry.is_dir() {
                continue;
            }
            if !safe_name(entry.name()) || !names.insert(entry.name().to_owned()) {
                return Err(invalid("Unsafe or duplicate asset archive path"));
            }
            if entry.size() > MAX_RESOURCE {
                return Err(invalid("Asset archive entry exceeds size limit"));
            }
        }
        Ok(Self {
            archive: Mutex::new(archive),
        })
    }
}

impl Pack for ArchivePack {
    fn open(&self, name: &str) -> Option<Box<dyn Read>> {
        if !name.starts_with("assets/") || !safe_name(name) {
            return None;
        }
        let mut archive = self.archive.lock().ok()?;
        let entry = archive.by_name(name).ok()?;
        if entry.size() > MAX_RESOURCE {
            return None;
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.take(MAX_RESOURCE + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > MAX_RESOURCE {
            return None;
        }
        Some(Box::new(Cursor::new(bytes)))
    }
}

/// The objects directory is beside indexes/, not relative to the process CWD.
pub struct IndexedPack {
    objects_root: PathBuf,
    objects: HashMap<String, String>,
}

impl IndexedPack {
    pub fn from_index(index: &Path) -> io::Result<Self> {
        let index = index.canonicalize()?;
        let objects_root = index
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("Asset index must have an asset root"))?
            .join("objects");
        let mut bytes = Vec::new();
        File::open(&index)?
            .take(MAX_INDEX + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_INDEX {
            return Err(invalid("Asset index too large"));
        }
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid asset index JSON"))?;
        let entries = json
            .get("objects")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| invalid("Asset index has no objects map"))?;
        if entries.len() > 500_000 {
            return Err(invalid("Too many indexed assets"));
        }
        let mut objects = HashMap::new();
        for (name, entry) in entries {
            let hash = entry
                .get("hash")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("Indexed asset has no hash"))?;
            if !safe_name(name)
                || hash.len() != 40
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(invalid("Invalid indexed asset name or hash"));
            }
            objects.insert(name.clone(), hash.to_owned());
        }
        Ok(Self {
            objects_root,
            objects,
        })
    }
}

impl Pack for IndexedPack {
    fn open(&self, name: &str) -> Option<Box<dyn Read>> {
        let name = name.strip_prefix("assets/")?;
        if !safe_name(name) {
            return None;
        }
        let hash = self.objects.get(name)?;
        let file = File::open(self.objects_root.join(&hash[..2]).join(hash)).ok()?;
        if file.metadata().ok()?.len() > MAX_RESOURCE {
            return None;
        }
        Some(Box::new(file.take(MAX_RESOURCE)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "leafish-assets-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            // Only the newly created test directory, never an arbitrary path.
            let parent = std::env::temp_dir().canonicalize().ok();
            if let Ok(path) = self.0.canonicalize() {
                if path.parent() == parent.as_deref()
                    && path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("leafish-assets-")
                {
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }
    }
    fn zip(path: &Path, names: &[(&str, &[u8])]) {
        let mut out = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in names {
            out.start_file(*name, zip::write::FileOptions::default())
                .unwrap();
            out.write_all(bytes).unwrap();
        }
        out.finish().unwrap();
    }
    #[test]
    fn archive_reads_exact_asset_without_extracting() {
        let dir = Temp::new();
        let path = dir.0.join("client.jar");
        zip(
            &path,
            &[
                ("assets/minecraft/blockstates/stone.json", b"modern"),
                ("version.json", b"not an asset"),
            ],
        );
        let before = std::fs::read(&path).unwrap();
        let pack = ArchivePack::open_archive(&path).unwrap();
        let mut data = String::new();
        pack.open("assets/minecraft/blockstates/stone.json")
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "modern");
        assert!(pack.open("version.json").is_none());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[test]
    fn archive_rejects_unsafe_and_duplicate_asset_names() {
        let dir = Temp::new();
        let path = dir.0.join("bad.jar");
        zip(&path, &[("assets/minecraft/../../escape", b"x")]);
        assert!(ArchivePack::open_archive(&path).is_err());
        zip(&path, &[("assets/x/test", b"a"), ("assets/x/test", b"b")]);
        assert!(ArchivePack::open_archive(&path).is_err());
    }
    #[test]
    fn explicit_sources_are_local_and_last_archive_wins() {
        let dir = Temp::new();
        let vanilla = dir.0.join("client.jar");
        let resource_pack = dir.0.join("pack.zip");
        zip(
            &vanilla,
            &[("assets/minecraft/test.txt", b"supplied vanilla")],
        );
        zip(
            &resource_pack,
            &[("assets/minecraft/test.txt", b"selected resource pack")],
        );
        let (manager, _) =
            super::super::Manager::from_local_sources(&vanilla, None, &[resource_pack]).unwrap();
        assert_eq!(manager.pending_downloads.load(Ordering::Acquire), 0);
        let mut value = String::new();
        manager
            .open("minecraft", "test.txt")
            .unwrap()
            .read_to_string(&mut value)
            .unwrap();
        assert_eq!(value, "selected resource pack");
        let (manager, _) =
            super::super::Manager::new(None, Some(vanilla.to_str().unwrap().to_owned()));
        assert_eq!(manager.pending_downloads.load(Ordering::Acquire), 0);
        value.clear();
        manager
            .open("minecraft", "test.txt")
            .unwrap()
            .read_to_string(&mut value)
            .unwrap();
        assert_eq!(value, "supplied vanilla");
    }
    #[test]
    fn object_location_follows_supplied_index_and_rejects_bad_hashes() {
        let dir = Temp::new();
        std::fs::create_dir_all(dir.0.join("indexes")).unwrap();
        std::fs::create_dir_all(dir.0.join("objects/01")).unwrap();
        let hash = "0123456789abcdef0123456789abcdef01234567";
        std::fs::write(dir.0.join("objects/01").join(hash), b"local object").unwrap();
        let path = dir.0.join("indexes/test.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"objects":{{"minecraft/lang/test.json":{{"hash":"{}"}}}}}}"#,
                hash
            ),
        )
        .unwrap();
        let pack = IndexedPack::from_index(&path).unwrap();
        let mut bytes = Vec::new();
        pack.open("assets/minecraft/lang/test.json")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"local object");
        std::fs::write(
            &path,
            br#"{"objects":{"minecraft/test":{"hash":"../bad"}}}"#,
        )
        .unwrap();
        assert!(IndexedPack::from_index(&path).is_err());
    }
}
