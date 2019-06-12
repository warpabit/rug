use std::path::{Path, PathBuf};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::cmp;
use std::str;
use std::collections::BTreeMap;
use crypto::digest::Digest;
use crypto::sha1::Sha1;
use std::io::{self, ErrorKind, Read, Write};
use std::convert::TryInto;

use crate::lockfile::Lockfile;
use crate::util::*;

const MAX_PATH_SIZE: u16 = 0xfff;
const CHECKSUM_SIZE: u64 = 20;

const HEADER_SIZE: usize = 12;  // bytes
const MIN_ENTRY_SIZE: usize = 64;

#[derive(Debug, Clone)]
pub struct Entry {
    ctime: i64,
    ctime_nsec: i64,
    mtime: i64,
    mtime_nsec: i64,
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    oid: String,
    flags: u16,
    path: String,
}

impl Entry {
    fn is_executable(mode: u32) -> bool {
        (mode >> 6) & 0b1 == 1
    }

    fn mode(mode: u32) -> u32 {
        if Entry::is_executable(mode) {
            0o100755u32
        } else {
            0o100644u32
        }
    }
    
    fn new(pathname: &str, oid: &str, metadata: fs::Metadata) -> Entry {
        let path = pathname.to_string();
        Entry {
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: Entry::mode(metadata.mode()),
            uid: metadata.uid(),
            gid: metadata.gid(),
            size: metadata.size(),
            oid: oid.to_string(),
            flags: cmp::min(path.len() as u16, MAX_PATH_SIZE),
            path,
        }
    }

    fn parse(bytes: &[u8]) -> Result<Entry, std::io::Error> {
        let mut metadata_ints : Vec<u32> = vec![];
        for i in 0..10 {
            println!("{} .. {}", i*4, i*4 + 4);
            metadata_ints.push(
                u32::from_be_bytes(bytes[i*4 .. i*4 + 4]
                                   .try_into().unwrap())
            );
            println!("{:?}", metadata_ints);
        };

        let oid = encode_hex(&bytes[40..60]);
        let flags = u16::from_be_bytes(bytes[60..62]
                                       .try_into().unwrap());
        let path_bytes = bytes[62..].split(|b| b == &0u8)
            .next().unwrap();
        let path = str::from_utf8(path_bytes)
            .unwrap().to_string();

        Ok(Entry {
            ctime: metadata_ints[0] as i64,
            ctime_nsec: metadata_ints[1] as i64,
            mtime: metadata_ints[2] as i64,
            mtime_nsec: metadata_ints[3] as i64,
            dev: metadata_ints[4] as u64,
            ino: metadata_ints[5] as u64,
            mode: metadata_ints[6],
            uid: metadata_ints[7],
            gid: metadata_ints[8],
            size: metadata_ints[9] as u64,

            oid,
            flags,
            path
        })
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        // 10 32-bit integers
        bytes.extend_from_slice(&(self.ctime as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.ctime_nsec as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.mtime as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.mtime_nsec as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.dev as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.ino as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.mode as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.uid as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.gid as u32).to_be_bytes());
        bytes.extend_from_slice(&(self.size as u32).to_be_bytes());

        // 20 bytes (40-char hex-string)
        bytes.extend_from_slice(&decode_hex(&self.oid).expect("invalid oid"));

        // 16-bit
        bytes.extend_from_slice(&self.flags.to_be_bytes());

        bytes.extend_from_slice(self.path.as_bytes());
        bytes.push(0x0);

        // add padding
        while bytes.len() % 8 != 0 {
            bytes.push(0x0)
        }

        bytes
    }
}

pub struct Checksum {
    file: File,
    digest: Sha1,
}

impl Checksum {
    fn new(file: File) -> Checksum {
        Checksum { file,
                   digest: Sha1::new(),
        }
    }

    fn read(&mut self, size: usize) -> Result<Vec<u8>, std::io::Error> {
        let mut buf = vec![0; size];
        self.file.read_exact(&mut buf)?;

        Ok(buf)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), std::io::Error> {
        self.file.write(data)?;
        self.digest.input(data);

        Ok(())
    }

    fn write_checksum(&mut self) -> Result<(), std::io::Error> {
        self.file.write(self.digest.result_str().as_bytes())?;

        Ok(())
    }

    fn verify_checksum(&mut self) -> Result<(), std::io::Error> {
        let hash = self.digest.result_str();

        let mut buf = vec![0; CHECKSUM_SIZE as usize];
        self.file.read_exact(&mut buf)?;

        let sum = encode_hex(&buf);

        println!("hash: {}", hash);
        println!("sum: {}", sum);

        if sum != hash {
            return Err(io::Error::new(ErrorKind::Other,
                                      "Checksum does not match value stored on disk"));
        }

        Ok(())
    }
}

pub struct Index {
    pathname: PathBuf,
    entries: BTreeMap<String, Entry>,
    lockfile: Lockfile,
    hasher: Option<Sha1>,
    changed: bool,
}

impl Index {
    pub fn new(path: &Path) -> Index {
        Index { pathname: path.to_path_buf(),
                entries: BTreeMap::new(),
                lockfile: Lockfile::new(path),
                hasher: None,
                changed: false,
        }
    }

    pub fn begin_write(&mut self) {
        self.hasher = Some(Sha1::new());
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), std::io::Error> {
        self.lockfile.write_bytes(data)?;
        self.hasher.expect("Sha1 hasher not initialized").input(data);

        Ok(())
    }

    pub fn finish_write(&mut self) -> Result<(), std::io::Error> {
        let hash = self.hasher
            .expect("Sha1 hasher not initialized")
            .result_str();
        self.lockfile.write_bytes(&decode_hex(&hash).expect("invalid sha1"))?;
        self.lockfile.commit()?;

        Ok(())
    }

    pub fn write_updates(mut self) -> Result<(), std::io::Error> {
        self.lockfile.hold_for_update()?;

        let mut header_bytes : Vec<u8> = vec![];
        header_bytes.extend_from_slice(b"DIRC");
        header_bytes.extend_from_slice(&2u32.to_be_bytes()); // version no.
        header_bytes.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        self.begin_write();
        self.write(&header_bytes)?;
        for (_key, entry) in self.entries.clone().iter() {
            self.write(&entry.to_bytes())?;
        }
        self.finish_write()?;
        Ok(())
    }

    pub fn add(&mut self, pathname: &str, oid: &str, metadata: fs::Metadata) {
        let entry = Entry::new(pathname, oid, metadata);
        self.store_entry(entry);
        self.changed = true;
    }

    pub fn store_entry(&mut self, entry: Entry) {
        self.entries.insert(entry.path.clone(), entry);
    }

    pub fn load_for_update(&mut self) -> Result<(), std::io::Error> {
        self.lockfile.hold_for_update()?;
        self.load()?;

        Ok(())
    }

    fn clear(&mut self) {
        self.entries = BTreeMap::new();
        self.hasher = None;
        self.changed = false;
    }

    fn open_index_file(&self) -> Option<File> {
        if self.pathname.exists() {
            OpenOptions::new()
                .read(true)
                .open(self.pathname.clone())
                .ok()
        } else {
            None
        }
    }

    fn read_header(checksum: &mut Checksum) -> usize {
        let data = checksum.read(HEADER_SIZE)
            .expect("could not read checksum header");
        let signature = str::from_utf8(&data[0..4])
            .expect("invalid signature");
        let version = u32::from_be_bytes(data[4..8]
                                         .try_into().unwrap());
        let count = u32::from_be_bytes(data[8..12]
                                       .try_into().unwrap());

        if signature != "DIRC" {
            panic!("Signature: expected 'DIRC', but found {}",
                   signature);
        }

        if version != 2 {
            panic!("Version: expected '2', but found {}",
                   version);
        }

        count as usize
    }

    fn read_entries(&mut self, checksum: &mut Checksum, count: usize) -> Result<(), std::io::Error> {
        for _i in 0..count {
            let mut entry = checksum.read(MIN_ENTRY_SIZE)?;
            while entry.last().unwrap() != &0u8 {
                entry.extend_from_slice(&checksum.read(8)?);
            }

            println!("entry: {:?}", entry);
            self.store_entry(Entry::parse(&entry)?);
        }

        Ok(())
    }

    fn load(&mut self) -> Result<(), std::io::Error> {
        self.clear();
        if let Some(file) = self.open_index_file() {
            let mut reader = Checksum::new(file);
            let count = Index::read_header(&mut reader);
            self.read_entries(&mut reader, count)?;
            reader.verify_checksum()?;
        }

        Ok(())
    }
}
