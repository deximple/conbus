//! Getting one file to the destination, and being able to say it arrived.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{Layout, MANIFEST, RECORD, Row, file_name, named};
use crate::sync::hex;

const CHUNK: usize = 1 << 20;

/// One segment to copy.
pub(super) struct Job {
    pub source: std::path::PathBuf,
    pub name: String,
    pub size: u64,
}

/// A file's size and hash.
#[derive(PartialEq, Eq)]
pub(super) struct Digested {
    size: u64,
    pub sha256: String,
}

impl Digested {
    pub fn into_row(self, name: &str) -> Row {
        Row {
            name: name.to_string(),
            size: self.size,
            sha256: self.sha256,
        }
    }
}

/// Copy one segment under a temporary name, read it back, and only then give it
/// its real name. Says whether it was a clone.
pub(super) fn copy_verified(job: &Job, log: &Path, clones: bool) -> io::Result<(Row, bool)> {
    copy_with(job, log, clones, write_plain)
}

fn copy_with(
    job: &Job,
    log: &Path,
    clones: bool,
    write: impl Fn(&Path, &Path) -> io::Result<Digested>,
) -> io::Result<(Row, bool)> {
    let temp = log.join(temp_name(&job.name));
    let placed = place_and_check(job, &temp, clones, write).and_then(|placed| {
        fs::rename(&temp, log.join(&job.name))?;
        Ok(placed)
    });
    if placed.is_err() {
        let _ = fs::remove_file(&temp);
    }
    placed.map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", job.name)))
}

fn place_and_check(
    job: &Job,
    temp: &Path,
    clones: bool,
    write: impl Fn(&Path, &Path) -> io::Result<Digested>,
) -> io::Result<(Row, bool)> {
    let cloned = clones && clone_file(&job.source, temp)?;
    let written = if cloned {
        // A clone takes the source's mode, and a log from before 0.2.5 can
        // still hold 0644 segments.
        fs::set_permissions(temp, fs::Permissions::from_mode(0o600))?;
        digest_file(&job.source)?
    } else {
        write(&job.source, temp)?
    };
    check_read_back(temp, &written)?;
    Ok((written.into_row(&job.name), cloned))
}

fn check_read_back(temp: &Path, written: &Digested) -> io::Result<()> {
    let found = digest_file(temp)?;
    if found == *written {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "the copy did not read back as written ({} bytes went in, {} came out, or the \
             bytes changed), and was discarded",
            written.size, found.size
        )))
    }
}

/// A clone rather than `std::fs::copy`, which on macOS already tries
/// `fclonefileat` and quietly falls back to a real copy: it returns a byte count
/// either way, so a caller using it cannot say which one happened. `false` means
/// no clone is possible here, and a real copy is the next thing to try.
#[cfg(target_os = "macos")]
fn clone_file(source: &Path, temp: &Path) -> io::Result<bool> {
    let (from, to) = (c_path(source)?, c_path(temp)?);
    // SAFETY: both pointers come from `CString`s that live across the call, and
    // `clonefile` keeps neither.
    if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) } == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ENOTSUP | libc::EXDEV) => Ok(false),
        _ => Err(err),
    }
}

#[cfg(not(target_os = "macos"))]
fn clone_file(_source: &Path, _temp: &Path) -> io::Result<bool> {
    Ok(false)
}

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| named(path)(io::ErrorKind::InvalidInput.into()))
}

/// What the volume holding a path can do and where it is.
#[derive(Default)]
pub(super) struct Volume {
    /// It can make a clone: APFS, on a Mac.
    pub clones: bool,
    /// It is not local, so a copy there leaves this machine.
    pub network: bool,
    /// The disk it is on, where the system says. Two volumes in one APFS container
    /// share one, and fail together.
    pub disk: Option<String>,
}

#[cfg(target_os = "macos")]
pub(super) fn volume_of(path: &Path) -> Volume {
    let Ok(text) = c_path(path) else {
        return Volume::default();
    };
    // SAFETY: a zeroed `statfs` is a valid value of a plain C struct, and `text`
    // lives across the call, which fills in `stat` or fails.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(text.as_ptr(), &mut stat) } != 0 {
        return Volume::default();
    }
    // SAFETY: on success both are NUL-terminated names inside `stat`.
    let (kind, source) = unsafe {
        (
            std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()),
            std::ffi::CStr::from_ptr(stat.f_mntfromname.as_ptr()),
        )
    };
    Volume {
        clones: kind.to_bytes() == b"apfs",
        network: stat.f_flags & libc::MNT_LOCAL as u32 == 0,
        disk: source
            .to_str()
            .ok()
            .and_then(whole_disk)
            .map(str::to_string),
    }
}

/// The whole disk in a mount source, up to the `sN` suffix: `disk3` from
/// `/dev/disk3s5` and from `/dev/disk3s1s1`. Nothing for what is not a local disk.
#[cfg(target_os = "macos")]
fn whole_disk(source: &str) -> Option<&str> {
    let device = source.strip_prefix("/dev/")?;
    let digits = device.strip_prefix("disk")?;
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    (end > 0).then(|| &device[.."disk".len() + end])
}

#[cfg(not(target_os = "macos"))]
pub(super) fn volume_of(_path: &Path) -> Volume {
    Volume::default()
}

/// Hash everything `from` holds, handing each chunk to `each` on the way past.
fn digest_through(
    mut from: impl Read,
    mut each: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<Digested> {
    let mut hasher = Sha256::new();
    let mut size = 0;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => {
                return Ok(Digested {
                    size,
                    sha256: hex(&hasher.finalize()),
                });
            }
            Ok(n) => {
                hasher.update(&buffer[..n]);
                size += n as u64;
                each(&buffer[..n])?;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// The hash of a file's first `bytes` bytes.
pub(super) fn digest_start(path: &Path, bytes: u64) -> io::Result<Digested> {
    let file = File::open(path).map_err(named(path))?;
    skip_os_cache(&file);
    digest_through(file.take(bytes), |_| Ok(())).map_err(named(path))
}

pub(super) fn digest_file(path: &Path) -> io::Result<Digested> {
    digest_start(path, u64::MAX)
}

/// Copy the bytes and hash them in the same pass, so the hash is of what was
/// written. Not `std::fs::copy`, which would carry the source's mode and extended
/// attributes along.
fn write_plain(source: &Path, temp: &Path) -> io::Result<Digested> {
    let from = File::open(source).map_err(named(source))?;
    skip_os_cache(&from);
    let mut to = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp)
        .map_err(named(temp))?;
    skip_os_cache(&to);
    let written = digest_through(from, |chunk| to.write_all(chunk).map_err(named(temp)))?;
    flush_to_disk(&to).map_err(named(temp))?;
    Ok(written)
}

/// Ask the OS not to answer later reads of this file from memory: a read-back
/// served from the page cache proves nothing about the disk. Best effort, and a
/// hint either way. On a Mac it keeps this file's I/O out of the cache; on Linux
/// it drops the clean pages, so the read-back has to come after the flush.
#[cfg(target_os = "macos")]
fn skip_os_cache(file: &File) {
    // SAFETY: `file` is an open descriptor, and F_NOCACHE takes a plain integer.
    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
}

#[cfg(not(target_os = "macos"))]
fn skip_os_cache(file: &File) {
    // SAFETY: `file` is an open descriptor; the call touches no memory of ours.
    unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
}

/// `sync_all` is `F_FULLFSYNC` on a Mac, which not every volume implements: a
/// network share refuses it. A plain `fsync` is the most such a volume offers.
fn flush_to_disk(file: &File) -> io::Result<()> {
    if file.sync_all().is_ok() {
        return Ok(());
    }
    // SAFETY: `file` is an open descriptor; the call touches no memory of ours.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Make a rename durable. Best effort: some file systems cannot sync a directory,
/// and a rename lost to a crash only means the next run does that copy again.
pub(super) fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

fn temp_name(name: &str) -> String {
    format!(".{name}.{}.tmp", std::process::id())
}

fn is_leftover(name: &str) -> bool {
    name.strip_prefix('.')
        .filter(|rest| rest.ends_with(".tmp"))
        .is_some_and(|rest| {
            rest.starts_with("segment-") || rest.starts_with(MANIFEST) || rest.starts_with(RECORD)
        })
}

/// The lock is held, so any temporary file here is from a run that died.
pub(super) fn remove_leftovers(layout: &Layout, root: &Path) -> io::Result<()> {
    for dir in [root, layout.dir.as_path(), layout.log.as_path()] {
        for entry in fs::read_dir(dir).map_err(named(dir))? {
            let path = entry.map_err(named(dir))?.path();
            if is_leftover(&file_name(&path)) {
                fs::remove_file(&path).map_err(named(&path))?;
            }
        }
    }
    Ok(())
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = dir.join(temp_name(&file_name(path)));
    let written = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        flush_to_disk(&file)?;
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written.map_err(named(path))?;
    sync_dir(dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_own_temporary_names_count_as_leftovers() {
        assert!(is_leftover(".segment-00000001.jsonl.4242.tmp"));
        assert!(is_leftover(".manifest.json.4242.tmp"));
        for name in [
            "segment-00000001.jsonl",
            "manifest.json",
            ".DS_Store",
            "._segment-00000001.jsonl",
            ".someone-elses.tmp",
        ] {
            assert!(!is_leftover(name), "{name}");
        }
    }

    #[test]
    fn a_copy_that_reads_back_wrong_never_gets_its_real_name() {
        // A disk that takes the write and hands back something else. The hash
        // says what went in; the read-back has to be what catches it.
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("log");
        fs::create_dir(&log).expect("dir");
        let source = temp.path().join("source");
        fs::write(&source, b"what went in").expect("write");
        let job = Job {
            source: source.clone(),
            name: "segment-00000000.jsonl".into(),
            size: 12,
        };

        let err = copy_with(&job, &log, false, |source, temp| {
            fs::write(temp, b"what came out")?;
            digest_file(source)
        })
        .expect_err("a read-back that differs");

        assert!(err.to_string().contains("did not read back"), "{err}");
        assert_eq!(fs::read_dir(&log).expect("read").count(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_disk_is_its_mount_source_up_to_the_slice() {
        assert_eq!(whole_disk("/dev/disk3s5"), Some("disk3"));
        assert_eq!(whole_disk("/dev/disk3s1s1"), Some("disk3"));
        assert_eq!(whole_disk("/dev/disk14"), Some("disk14"));
        assert_eq!(whole_disk("/dev/disk"), None);
        assert_eq!(whole_disk("//me@nas/share"), None);
        assert_eq!(whole_disk("map auto_home"), None);
        assert_eq!(whole_disk("devfs"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn what_is_not_local_reads_as_leaving_this_machine() {
        // An automounter's map is the one non-local mount every Mac has.
        assert!(volume_of(Path::new("/System/Volumes/Data/home")).network);
        assert!(!volume_of(Path::new("/tmp")).network);
        assert!(volume_of(Path::new("/tmp")).disk.is_some());
    }

    #[test]
    fn a_copy_that_does_not_read_back_as_written_is_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let copy = temp.path().join("copy");
        fs::write(&copy, b"what is really there").expect("write");
        let other = temp.path().join("other");
        fs::write(&other, b"what went in").expect("write");
        let written = digest_file(&other).expect("digest");

        let err = check_read_back(&copy, &written).expect_err("must differ");

        assert!(err.to_string().contains("did not read back"), "{err}");
        let same = digest_file(&copy).expect("digest");
        check_read_back(&copy, &same).expect("the same bytes pass");
    }

    #[test]
    fn a_copy_that_fails_leaves_nothing_with_or_without_a_real_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("log");
        fs::create_dir(&log).expect("dir");
        let job = Job {
            source: temp.path().join("not-there"),
            name: "segment-00000000.jsonl".into(),
            size: 1,
        };

        assert!(copy_verified(&job, &log, false).is_err());

        assert_eq!(fs::read_dir(&log).expect("read").count(), 0);
    }

    #[test]
    fn hashing_the_start_of_a_file_is_hashing_that_part() {
        let temp = tempfile::tempdir().expect("tempdir");
        let whole = temp.path().join("whole");
        fs::write(&whole, b"prefix and the rest").expect("write");
        let part = temp.path().join("part");
        fs::write(&part, b"prefix").expect("write");

        let start = digest_start(&whole, 6).expect("digest");

        assert_eq!(start.size, 6);
        assert_eq!(start.sha256, digest_file(&part).expect("digest").sha256);
    }
}
