// Compress… and Extract Here: packing files and folders into a .zip, and
// unpacking one.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Like transfer.rs this is the engine only, run on the worker thread of
// jobs.rs with GIO's blocking calls, so it works on network shares too and
// never freezes the window. It reports progress through the same
// `Delegate` (it never asks anything: the name of a new archive was
// chosen in a dialog, and what an archive unpacks into always gets a free
// name).
//
// What it does, as GNOME Files and the Mac's Archive Utility do:
// - An archive unpacks next to itself: if everything in it sits in one
//   folder (or it holds one file), as that folder or file; otherwise into
//   a new folder named after the archive. A name already taken gets a
//   number ("Photos (2)"). The Mac's "__MACOSX" leftovers are left out.
// - Files that are compressed already (videos, pictures, other archives)
//   are stored as they are: squeezing them again costs time and saves
//   nothing.
//
// The rules that keep files safe:
// - Nothing is ever replaced: an archive is created new (the dialog made
//   sure its name is free), and an archive unpacks into a folder or file
//   made for it.
// - Entries that would land outside that place ("../x", "/etc/x") are
//   refused, and links stored in an archive are not unpacked (a link could
//   lead files unpacked after it out of the folder).
// - A half-written archive, or a half-unpacked file, from a failed or
//   cancelled run is removed again.

use std::collections::BTreeSet;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use gio::glib;
use gio::prelude::*;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::transfer::{self, Delegate, Progress};

const ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,standard::size,\
     standard::symlink-target,time::modified,unix::mode";
const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

/// Progress is reported at most this often (microseconds).
const PROGRESS_INTERVAL: i64 = if cfg!(test) { 0 } else { 50_000 };

/// How much is read and written at a time.
const CHUNK: usize = 256 * 1024;

/// What the Mac puts into the archives it makes, beside the files.
const MAC_LEFTOVERS: &str = "__MACOSX";

/// Endings of files that are compressed already: stored as they are.
const ALREADY_COMPRESSED: [&str; 34] = [
    "zip", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "jpg", "jpeg", "png", "gif", "webp", "heic", "avif",
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "mp3", "m4a", "aac", "ogg", "opus", "flac", "docx", "xlsx",
    "pptx", "odt", "ods", "jar", "apk",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Compress,
    Extract,
}

/// What to do.
#[derive(Clone, Debug)]
pub enum Request {
    /// Pack `sources` (files and folders of one folder) into the new file
    /// `archive`.
    Compress { sources: Vec<gio::File>, archive: gio::File },
    /// Unpack each of `archives` into `dest_dir`.
    Extract { archives: Vec<gio::File>, dest_dir: gio::File },
}

impl Request {
    pub fn kind(&self) -> Kind {
        match self {
            Request::Compress { .. } => Kind::Compress,
            Request::Extract { .. } => Kind::Extract,
        }
    }
}

/// How it ended.
#[derive(Debug)]
pub struct Summary {
    /// What was made: the archive, or what each archive unpacked into.
    pub created: Vec<gio::File>,
    /// Every file that could not be packed or unpacked (by its address,
    /// or "archive.zip › inside/it.txt"), with the reason.
    pub failed: Vec<(String, glib::Error)>,
    pub cancelled: bool,
    /// Set when it had to give up (for example: the disk is full).
    pub stopped: Option<glib::Error>,
}

/// Carries out `request`. Blocks until done; cancel through `cancellable`.
pub fn run(request: &Request, cancellable: &gio::Cancellable, delegate: &mut dyn Delegate) -> Summary {
    let mut job = Job {
        cancellable,
        delegate,
        progress: Progress { scanning: true, ..Progress::default() },
        last_emit: 0,
        summary: Summary { created: Vec::new(), failed: Vec::new(), cancelled: false, stopped: None },
    };
    let result = match request {
        Request::Compress { sources, archive } => job.compress(sources, archive),
        Request::Extract { archives, dest_dir } => job.extract_all(archives, dest_dir),
    };
    match result {
        Ok(()) => {}
        Err(Stop::Cancelled) => job.summary.cancelled = true,
        Err(Stop::Fatal(error)) => job.summary.stopped = Some(error),
    }
    job.emit(true);
    job.summary
}

/// Why the whole operation ends early.
enum Stop {
    Cancelled,
    Fatal(glib::Error),
}

impl From<glib::Error> for Stop {
    fn from(error: glib::Error) -> Self {
        if error.matches(gio::IOErrorEnum::Cancelled) { Stop::Cancelled } else { Stop::Fatal(error) }
    }
}

/// Which end of a copy failed.
enum Pumped {
    Read(glib::Error),
    Write(glib::Error),
    Cancelled,
}

struct Job<'a> {
    cancellable: &'a gio::Cancellable,
    delegate: &'a mut dyn Delegate,
    progress: Progress,
    last_emit: i64,
    summary: Summary,
}

/// One thing to pack, found while looking through the sources.
struct Item {
    file: gio::File,
    /// Its name inside the archive ("Photos/2024/a.jpg").
    name: String,
    what: What,
    /// Permissions (unix::mode), if the file system has them.
    mode: Option<u32>,
    modified: Option<glib::DateTime>,
}

enum What {
    Folder,
    File(u64),
    Link(String),
}

impl Job<'_> {
    fn emit(&mut self, force: bool) {
        let now = glib::monotonic_time();
        if force || now - self.last_emit >= PROGRESS_INTERVAL {
            self.last_emit = now;
            self.delegate.progress(&self.progress);
        }
    }

    fn check_cancelled(&self) -> Result<(), Stop> {
        if self.cancellable.is_cancelled() { Err(Stop::Cancelled) } else { Ok(()) }
    }

    fn fail(&mut self, what: String, error: glib::Error) -> Result<(), Stop> {
        if error.matches(gio::IOErrorEnum::Cancelled) {
            return Err(Stop::Cancelled);
        }
        self.summary.failed.push((what, error));
        Ok(())
    }

    /// Copies everything from `from` to `to`, counting the bytes into the
    /// progress.
    fn pump(&mut self, from: &mut impl Read, to: &mut impl Write) -> Result<(), Pumped> {
        let mut buffer = vec![0; CHUNK];
        loop {
            if self.cancellable.is_cancelled() {
                return Err(Pumped::Cancelled);
            }
            let read = match from.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Pumped::Read(gio_error(error))),
            };
            to.write_all(&buffer[..read]).map_err(|error| Pumped::Write(gio_error(error)))?;
            self.progress.bytes_done += read as u64;
            self.emit(false);
        }
    }

    // --- Compress ---

    fn compress(&mut self, sources: &[gio::File], archive: &gio::File) -> Result<(), Stop> {
        let mut items = Vec::new();
        for source in sources {
            self.scan(source, None, &mut items)?;
        }
        self.progress.scanning = false;
        self.emit(true);

        let stream = archive.create(gio::FileCreateFlags::NONE, Some(self.cancellable))?;
        let written = self.write_archive(&items, &stream);
        let closed = stream.close(Some(self.cancellable));
        match written.and_then(|()| closed.map_err(Stop::from)) {
            Ok(()) => {
                self.summary.created.push(archive.clone());
                Ok(())
            }
            Err(stop) => {
                // Half an archive is no archive.
                let _ = archive.delete(gio::Cancellable::NONE);
                Err(stop)
            }
        }
    }

    /// Adds `file` (and, for a folder, everything in it) to `items`, named
    /// `inside/` + its name in the archive.
    fn scan(&mut self, file: &gio::File, inside: Option<&str>, items: &mut Vec<Item>) -> Result<(), Stop> {
        self.check_cancelled()?;
        let info = match file.query_info(ATTRIBUTES, NOFOLLOW, Some(self.cancellable)) {
            Ok(info) => info,
            Err(error) => return self.fail(file.uri().into(), error),
        };
        let own = info.name().to_string_lossy().into_owned();
        let name = inside.map_or_else(|| own.clone(), |inside| format!("{inside}/{own}"));
        let what = match info.file_type() {
            gio::FileType::Directory => What::Folder,
            gio::FileType::SymbolicLink => {
                What::Link(info.symlink_target().map(|target| target.to_string_lossy().into_owned()).unwrap_or_default())
            }
            gio::FileType::Regular => What::File(info.size().max(0) as u64),
            // Sockets, devices and the like are no files to pack.
            _ => return Ok(()),
        };
        let is_folder = matches!(what, What::Folder);
        if let What::File(size) = what {
            self.progress.bytes_total += size;
        }
        self.progress.files_total += 1;
        self.progress.current = info.display_name().into();
        self.emit(false);
        items.push(Item {
            file: file.clone(),
            name: name.clone(),
            what,
            mode: info.has_attribute("unix::mode").then(|| info.attribute_uint32("unix::mode")),
            modified: info.modification_date_time(),
        });
        if is_folder {
            let children = match file.enumerate_children(ATTRIBUTES, NOFOLLOW, Some(self.cancellable)) {
                Ok(children) => children,
                Err(error) => return self.fail(file.uri().into(), error),
            };
            loop {
                match children.next_file(Some(self.cancellable)) {
                    Ok(Some(child)) => self.scan(&children.child(&child), Some(&name), items)?,
                    Ok(None) => break,
                    Err(error) => {
                        self.fail(file.uri().into(), error)?;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn write_archive(&mut self, items: &[Item], stream: &gio::FileOutputStream) -> Result<(), Stop> {
        // Streamed: network shares can't always seek in what is written.
        let writer = GioWriter { stream: stream.clone().upcast(), cancellable: self.cancellable.clone() };
        let mut zip = ZipWriter::new_stream(writer);
        for item in items {
            self.check_cancelled()?;
            self.progress.current = item.name.rsplit('/').next().unwrap_or_default().to_string();
            let mut options = SimpleFileOptions::default().last_modified_time(zip_time(item.modified.as_ref()));
            if let Some(mode) = item.mode {
                options = options.unix_permissions(mode & 0o7777);
            }
            match &item.what {
                What::Folder => zip.add_directory(item.name.as_str(), options).map_err(fatal)?,
                What::Link(target) => zip.add_symlink(&item.name, target, options).map_err(fatal)?,
                What::File(size) => {
                    // Opened first: a file that can't be read is left out
                    // whole, instead of half in the archive.
                    let input = match item.file.read(Some(self.cancellable)) {
                        Ok(input) => input,
                        Err(error) => {
                            self.fail(item.file.uri().into(), error)?;
                            self.progress.bytes_done += size;
                            continue;
                        }
                    };
                    let method =
                        if already_compressed(&item.name) { CompressionMethod::Stored } else { CompressionMethod::Deflated };
                    let options = options.compression_method(method).large_file(*size >= u64::from(u32::MAX));
                    zip.start_file(item.name.as_str(), options).map_err(fatal)?;
                    let mut reader = GioReader::new(input.upcast(), None, self.cancellable);
                    match self.pump(&mut reader, &mut zip) {
                        Ok(()) => {}
                        Err(Pumped::Cancelled) => return Err(Stop::Cancelled),
                        Err(Pumped::Write(error)) => return Err(error.into()),
                        // The entry keeps what was read; the file counts
                        // as failed.
                        Err(Pumped::Read(error)) => self.fail(item.file.uri().into(), error)?,
                    }
                }
            }
            self.progress.files_done += 1;
            self.emit(false);
        }
        zip.finish().map_err(fatal)?;
        Ok(())
    }

    // --- Extract ---

    fn extract_all(&mut self, archives: &[gio::File], dest_dir: &gio::File) -> Result<(), Stop> {
        for archive in archives {
            self.check_cancelled()?;
            match self.extract(archive, dest_dir) {
                Ok(()) => {}
                // An archive that can't be read at all: on to the next.
                Err(Stop::Fatal(error)) if archives.len() > 1 => self.fail(archive.uri().into(), error)?,
                Err(stop) => return Err(stop),
            }
        }
        Ok(())
    }

    fn extract(&mut self, archive: &gio::File, dest_dir: &gio::File) -> Result<(), Stop> {
        let archive_name = archive.basename().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let input = archive.read(Some(self.cancellable))?;
        if !input.can_seek() {
            return Err(Stop::Fatal(glib::Error::new(
                gio::IOErrorEnum::NotSupported,
                "The archive can’t be read from where it is",
            )));
        }
        let seekable = input.clone().upcast::<gio::Seekable>();
        let reader = GioReader::new(input.upcast(), Some(seekable), self.cancellable);
        let mut zip = ZipArchive::new(reader).map_err(fatal)?;

        // What is in it, and where each entry goes.
        let mut entries = Vec::new();
        for index in 0..zip.len() {
            let entry = match zip.by_index(index) {
                Ok(entry) => entry,
                Err(error) => {
                    self.fail(format!("{archive_name} › #{}", index + 1), zip_error(error))?;
                    continue;
                }
            };
            let shown = entry.name().map(|name| name.into_owned()).unwrap_or_default();
            let Some(path) = entry.enclosed_name() else {
                let error = glib::Error::new(gio::IOErrorEnum::InvalidFilename, "It would land outside the folder");
                self.fail(format!("{archive_name} › {shown}"), error)?;
                continue;
            };
            if path.components().next().is_some_and(|first| first.as_os_str() == MAC_LEFTOVERS) {
                continue;
            }
            if !entry.is_dir() {
                self.progress.bytes_total += entry.size();
            }
            self.progress.files_total += 1;
            entries.push((index, path, shown, entry.is_dir(), entry.is_symlink()));
        }
        self.progress.scanning = false;
        self.emit(true);

        let paths: Vec<(&Path, bool)> = entries.iter().map(|(_, path, _, is_dir, _)| (path.as_path(), *is_dir)).collect();
        let (root, layout) = match layout(&paths, &archive_stem(&archive_name)) {
            Layout::Folder(name) => {
                let folder = free_child(dest_dir, &name, true, self.cancellable)?;
                folder.make_directory(Some(self.cancellable))?;
                (folder, Layout::Folder(name))
            }
            Layout::AsIs(top, is_folder) => (free_child(dest_dir, &top, is_folder, self.cancellable)?, Layout::AsIs(top, is_folder)),
        };
        self.summary.created.push(root.clone());

        for (index, path, shown, is_dir, is_link) in entries {
            self.check_cancelled()?;
            let what = format!("{archive_name} › {shown}");
            let target = match &layout {
                Layout::Folder(_) => descend(&root, &path),
                // The top item is `root` itself (maybe numbered).
                Layout::AsIs(..) => {
                    let rest: PathBuf = path.components().skip(1).collect();
                    descend(&root, &rest)
                }
            };
            self.progress.current = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            if is_link {
                let error = glib::Error::new(gio::IOErrorEnum::NotSupported, "Links in archives are not unpacked");
                self.fail(what, error)?;
            } else if is_dir {
                if let Err(error) = make_folders(&target, self.cancellable) {
                    self.fail(what, error)?;
                }
            } else {
                let mut entry = match zip.by_index(index) {
                    Ok(entry) => entry,
                    Err(error) => {
                        self.fail(what, zip_error(error))?;
                        continue;
                    }
                };
                let size = entry.size();
                let mode = entry.unix_mode();
                let modified = entry.last_modified().and_then(unix_time);
                if let Some(parent) = target.parent()
                    && let Err(error) = make_folders(&parent, self.cancellable)
                {
                    self.fail(what, error)?;
                    self.progress.bytes_done += size;
                    continue;
                }
                let out = match target.create(gio::FileCreateFlags::NONE, Some(self.cancellable)) {
                    Ok(out) => out,
                    Err(error) => {
                        self.fail(what, error)?;
                        self.progress.bytes_done += size;
                        continue;
                    }
                };
                let mut writer = GioWriter { stream: out.clone().upcast(), cancellable: self.cancellable.clone() };
                let pumped = self.pump(&mut entry, &mut writer);
                let closed = out.close(Some(self.cancellable));
                match (pumped, closed) {
                    (Ok(()), Ok(_)) => {
                        // Times and permissions as packed, where the file
                        // system has them (not worth failing over).
                        if let Some(seconds) = modified {
                            let _ = target.set_attribute_uint64("time::modified", seconds, NOFOLLOW, gio::Cancellable::NONE);
                        }
                        if let Some(mode) = mode {
                            let _ = target.set_attribute_uint32("unix::mode", mode & 0o7777, NOFOLLOW, gio::Cancellable::NONE);
                        }
                    }
                    (pumped, closed) => {
                        let _ = target.delete(gio::Cancellable::NONE);
                        match pumped {
                            Err(Pumped::Cancelled) => return Err(Stop::Cancelled),
                            // Can't write here: the rest won't fare better.
                            Err(Pumped::Write(error)) => return Err(error.into()),
                            Err(Pumped::Read(error)) => self.fail(what, error)?,
                            Ok(()) => {
                                if let Err(error) = closed {
                                    return Err(error.into());
                                }
                            }
                        }
                    }
                }
            }
            self.progress.files_done += 1;
            self.emit(false);
        }
        Ok(())
    }
}

// --- Where things go ---

/// How an archive unpacks.
#[derive(Debug, PartialEq, Eq)]
enum Layout {
    /// Into a new folder of this name (the archive's name without ".zip").
    Folder(String),
    /// Everything is in one folder, or it is one file (the bool: a folder):
    /// that item comes out as it is.
    AsIs(String, bool),
}

/// How an archive whose entries are `paths` (each with whether it is a
/// folder) unpacks; `stem` is the archive's name without ".zip".
fn layout(paths: &[(&Path, bool)], stem: &str) -> Layout {
    let tops: BTreeSet<&std::ffi::OsStr> =
        paths.iter().filter_map(|(path, _)| path.components().next()).map(Component::as_os_str).collect();
    match tops.into_iter().collect::<Vec<_>>()[..] {
        [top] => {
            // A folder if anything lies inside it, or it was stored as one.
            let is_folder = paths.iter().any(|(path, is_dir)| *is_dir || path.components().count() > 1);
            Layout::AsIs(top.to_string_lossy().into_owned(), is_folder)
        }
        _ => Layout::Folder(stem.to_string()),
    }
}

/// The archive's name without ".zip" ("Photos.zip" → "Photos").
fn archive_stem(name: &str) -> String {
    let lower = name.to_lowercase();
    match lower.strip_suffix(".zip") {
        Some(stem) if !stem.is_empty() => name[..stem.len()].to_string(),
        _ => format!("{name} contents"),
    }
}

/// Whether the file `name` is compressed already (by its ending).
fn already_compressed(name: &str) -> bool {
    let name = name.rsplit('/').next().unwrap_or(name);
    name.rsplit_once('.')
        .is_some_and(|(stem, ending)| !stem.is_empty() && ALREADY_COMPRESSED.contains(&ending.to_lowercase().as_str()))
}

/// `name` inside `dest_dir`, numbered if taken ("Photos (2)").
fn free_child(dest_dir: &gio::File, name: &str, is_folder: bool, cancellable: &gio::Cancellable) -> Result<gio::File, Stop> {
    let mut child = dest_dir.child(name);
    let mut number = 2;
    while child.query_exists(Some(cancellable)) {
        if cancellable.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        child = dest_dir.child(transfer::numbered(Path::new(name), number, is_folder));
        number += 1;
    }
    Ok(child)
}

/// `path` (relative, checked by `enclosed_name`) below `root`.
fn descend(root: &gio::File, path: &Path) -> gio::File {
    path.components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .fold(root.clone(), |file, name| file.child(name))
}

/// Makes `folder` and the folders above it that are missing.
fn make_folders(folder: &gio::File, cancellable: &gio::Cancellable) -> Result<(), glib::Error> {
    match folder.make_directory_with_parents(Some(cancellable)) {
        Err(error) if error.matches(gio::IOErrorEnum::Exists) => Ok(()),
        other => other,
    }
}

// --- Times ---

/// A file's modification time as a zip stores it (local time, from 1980).
fn zip_time(modified: Option<&glib::DateTime>) -> zip::DateTime {
    modified
        .and_then(|time| time.to_local().ok())
        .and_then(|t| {
            zip::DateTime::from_date_and_time(
                u16::try_from(t.year()).ok()?,
                t.month() as u8,
                t.day_of_month() as u8,
                t.hour() as u8,
                t.minute() as u8,
                t.second() as u8,
            )
            .ok()
        })
        .unwrap_or_else(zip::DateTime::default_for_write)
}

/// A zip's time as seconds since 1970.
fn unix_time(time: zip::DateTime) -> Option<u64> {
    let local = glib::DateTime::from_local(
        i32::from(time.year()),
        i32::from(time.month()),
        i32::from(time.day()),
        i32::from(time.hour()),
        i32::from(time.minute()),
        f64::from(time.second()),
    )
    .ok()?;
    u64::try_from(local.to_unix()).ok()
}

// --- GIO streams as Rust's Read, Write and Seek ---

/// Reads through GIO, so cancelling stops a read that hangs on a share.
struct GioReader {
    stream: gio::InputStream,
    seekable: Option<gio::Seekable>,
    cancellable: gio::Cancellable,
}

impl GioReader {
    fn new(stream: gio::InputStream, seekable: Option<gio::Seekable>, cancellable: &gio::Cancellable) -> Self {
        Self { stream, seekable, cancellable: cancellable.clone() }
    }
}

impl Read for GioReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buffer, Some(&self.cancellable)).map_err(io::Error::other)
    }
}

impl Seek for GioReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let seekable = self.seekable.as_ref().ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))?;
        let (offset, kind) = match to {
            SeekFrom::Start(offset) => (i64::try_from(offset).map_err(io::Error::other)?, glib::SeekType::Set),
            SeekFrom::Current(offset) => (offset, glib::SeekType::Cur),
            SeekFrom::End(offset) => (offset, glib::SeekType::End),
        };
        seekable.seek(offset, kind, Some(&self.cancellable)).map_err(io::Error::other)?;
        Ok(seekable.tell().max(0) as u64)
    }
}

struct GioWriter {
    stream: gio::OutputStream,
    cancellable: gio::Cancellable,
}

impl Write for GioWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.stream.write(buffer, Some(&self.cancellable)).map_err(io::Error::other)?;
        Ok(written.max(0) as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush(Some(&self.cancellable)).map_err(io::Error::other)
    }
}

/// The GIO error inside `error`, or one saying what it says.
fn gio_error(error: io::Error) -> glib::Error {
    if let Some(inner) = error.get_ref().and_then(|inner| inner.downcast_ref::<glib::Error>()) {
        return inner.clone();
    }
    glib::Error::new(gio::IOErrorEnum::Failed, &error.to_string())
}

/// A zip error as a GIO error with a plain sentence (the library's own
/// words in brackets, without ": ", which `dialogs::reason` would cut at).
fn zip_error(error: zip::result::ZipError) -> glib::Error {
    use zip::result::ZipError;
    let detail = |error: &ZipError| error.to_string().replace(": ", " – ");
    match error {
        ZipError::Io(error) => gio_error(error),
        ZipError::InvalidArchive(_) => glib::Error::new(
            gio::IOErrorEnum::InvalidData,
            &format!("It isn’t a zip archive, or it is damaged ({})", detail(&error)),
        ),
        other => glib::Error::new(
            gio::IOErrorEnum::NotSupported,
            &format!("It uses something Kommander can’t unpack ({})", detail(&other)),
        ),
    }
}

fn fatal(error: zip::result::ZipError) -> Stop {
    zip_error(error).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kommander-archive-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }

        fn file(&self, relative: &str) -> gio::File {
            gio::File::for_path(self.path(relative))
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn read(&self, relative: &str) -> String {
            fs::read_to_string(self.path(relative)).unwrap()
        }

        fn names(&self, relative: &str) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.path(relative))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct Recorder(Vec<Progress>);

    impl Delegate for Recorder {
        fn progress(&mut self, progress: &Progress) {
            self.0.push(progress.clone());
        }

        fn conflict(&mut self, _: &transfer::Conflict) -> transfer::Answer {
            panic!("archives never ask")
        }
    }

    fn go(request: Request) -> (Summary, Recorder) {
        // Archives unpack into a panel's folder, which is there.
        if let Request::Extract { dest_dir, .. } = &request {
            let _ = dest_dir.make_directory_with_parents(gio::Cancellable::NONE);
        }
        let mut recorder = Recorder::default();
        let summary = run(&request, &gio::Cancellable::new(), &mut recorder);
        (summary, recorder)
    }

    #[test]
    fn compresses_and_extracts_again() {
        let t = Temp::new("round-trip");
        t.write("src/Photos/2024/a.txt", "aaaa");
        t.write("src/Photos/b.mp4", "not really a video");
        t.write("src/notes.txt", "notes");
        fs::create_dir_all(t.path("src/Photos/empty")).unwrap();
        let (summary, recorder) = go(Request::Compress {
            sources: vec![t.file("src/Photos"), t.file("src/notes.txt")],
            archive: t.file("src/Archive.zip"),
        });
        assert!(summary.failed.is_empty() && !summary.cancelled && summary.stopped.is_none(), "{summary:?}");
        assert_eq!(summary.created.len(), 1);
        let last = recorder.0.last().unwrap();
        assert_eq!((last.files_done, last.files_total), (6, 6));
        assert_eq!(last.bytes_done, last.bytes_total);

        // Two things at the top: they unpack into a folder named after it.
        let (summary, _) =
            go(Request::Extract { archives: vec![t.file("src/Archive.zip")], dest_dir: t.file("out") });
        assert!(summary.failed.is_empty(), "{:?}", summary.failed);
        assert_eq!(t.names("out"), ["Archive"]);
        assert_eq!(t.read("out/Archive/Photos/2024/a.txt"), "aaaa");
        assert_eq!(t.read("out/Archive/Photos/b.mp4"), "not really a video");
        assert_eq!(t.read("out/Archive/notes.txt"), "notes");
        assert!(t.path("out/Archive/Photos/empty").is_dir());

        // Again: a new name, nothing replaced.
        go(Request::Extract { archives: vec![t.file("src/Archive.zip")], dest_dir: t.file("out") });
        assert_eq!(t.names("out"), ["Archive", "Archive (2)"]);
    }

    #[test]
    fn one_folder_inside_comes_out_as_it_is() {
        let t = Temp::new("one-folder");
        t.write("src/Photos/a.txt", "a");
        go(Request::Compress { sources: vec![t.file("src/Photos")], archive: t.file("Photos.zip") });
        t.write("out/Photos/old.txt", "keep me");
        let (summary, _) = go(Request::Extract { archives: vec![t.file("Photos.zip")], dest_dir: t.file("out") });
        assert!(summary.created.len() == 1 && summary.created[0].equal(&t.file("out/Photos (2)")));
        assert_eq!(t.read("out/Photos (2)/a.txt"), "a");
        assert_eq!(t.read("out/Photos/old.txt"), "keep me");
    }

    #[test]
    fn a_single_file_keeps_its_name() {
        let t = Temp::new("one-file");
        t.write("report.pdf", "pdf");
        go(Request::Compress { sources: vec![t.file("report.pdf")], archive: t.file("report.zip") });
        go(Request::Extract { archives: vec![t.file("report.zip")], dest_dir: t.file("out") });
        assert_eq!(t.read("out/report.pdf"), "pdf");
        go(Request::Extract { archives: vec![t.file("report.zip")], dest_dir: t.file("out") });
        assert_eq!(t.names("out"), ["report (2).pdf", "report.pdf"]);
    }

    #[test]
    fn an_existing_archive_is_never_replaced() {
        let t = Temp::new("exists");
        t.write("a.txt", "a");
        t.write("a.zip", "precious");
        let (summary, _) = go(Request::Compress { sources: vec![t.file("a.txt")], archive: t.file("a.zip") });
        assert!(summary.stopped.is_some_and(|error| error.matches(gio::IOErrorEnum::Exists)));
        assert_eq!(t.read("a.zip"), "precious");
    }

    #[test]
    fn cancelling_removes_the_half_archive() {
        let t = Temp::new("cancel");
        t.write("a.txt", "a");
        let cancellable = gio::Cancellable::new();
        cancellable.cancel();
        let request = Request::Compress { sources: vec![t.file("a.txt")], archive: t.file("a.zip") };
        let summary = run(&request, &cancellable, &mut Recorder::default());
        assert!(summary.cancelled);
        assert!(!t.path("a.zip").exists());
    }

    /// Writes a zip with the given entries straight away (as another
    /// program might have).
    fn handmade(path: &Path, entries: &[(&str, &str)]) {
        let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
        for (name, content) in entries {
            if let Some(dir) = name.strip_suffix('/') {
                zip.add_directory(dir, SimpleFileOptions::default()).unwrap();
            } else {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(content.as_bytes()).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    #[test]
    fn entries_outside_the_folder_are_refused() {
        let t = Temp::new("escape");
        handmade(&t.path("evil.zip"), &[("../escaped.txt", "x"), ("/tmp/absolute.txt", "x"), ("fine.txt", "ok")]);
        let (summary, _) = go(Request::Extract { archives: vec![t.file("evil.zip")], dest_dir: t.file("out") });
        assert!(!t.path("escaped.txt").exists());
        assert!(summary.failed.iter().any(|(what, _)| what.contains("../escaped.txt")), "{:?}", summary.failed);
        // GNU-style "/tmp/absolute.txt" is read as "tmp/absolute.txt", inside.
        let names = t.names("out");
        assert!(names.iter().all(|name| ["evil", "fine.txt", "tmp"].contains(&name.as_str())), "{names:?}");
    }

    #[test]
    fn mac_leftovers_are_left_out() {
        let t = Temp::new("macosx");
        handmade(&t.path("mac.zip"), &[("Photos/a.txt", "a"), ("__MACOSX/Photos/._a.txt", "junk")]);
        go(Request::Extract { archives: vec![t.file("mac.zip")], dest_dir: t.file("out") });
        assert_eq!(t.names("out"), ["Photos"]);
        assert_eq!(t.read("out/Photos/a.txt"), "a");
    }

    #[test]
    fn a_broken_archive_says_so() {
        // In plain words, the library's in brackets.
        let t = Temp::new("broken");
        t.write("broken.zip", "this is no zip");
        let (summary, _) = go(Request::Extract { archives: vec![t.file("broken.zip")], dest_dir: t.file("out") });
        let error = summary.stopped.unwrap();
        assert!(error.message().starts_with("It isn’t a zip archive, or it is damaged ("), "{error}");
        assert!(!error.message().contains(": "));
        assert!(summary.created.is_empty());
    }

    #[test]
    fn layouts() {
        // A name ending in "/" stands for a folder, as zips store them.
        let run = |paths: &[&str]| {
            let paths: Vec<(PathBuf, bool)> =
                paths.iter().map(|path| (PathBuf::from(path.trim_end_matches('/')), path.ends_with('/'))).collect();
            layout(&paths.iter().map(|(path, dir)| (path.as_path(), *dir)).collect::<Vec<_>>(), "Archive")
        };
        assert_eq!(run(&["a.txt", "b.txt"]), Layout::Folder("Archive".into()));
        assert_eq!(run(&["Photos/", "Photos/a.jpg"]), Layout::AsIs("Photos".into(), true));
        assert_eq!(run(&["Photos/a.jpg"]), Layout::AsIs("Photos".into(), true));
        assert_eq!(run(&["Empty/"]), Layout::AsIs("Empty".into(), true));
        assert_eq!(run(&["report.pdf"]), Layout::AsIs("report.pdf".into(), false));
        assert_eq!(run(&[]), Layout::Folder("Archive".into()));
    }

    #[test]
    fn names_and_endings() {
        assert_eq!(archive_stem("Photos.zip"), "Photos");
        assert_eq!(archive_stem("BACKUP.ZIP"), "BACKUP");
        assert_eq!(archive_stem(".zip"), ".zip contents");
        assert!(already_compressed("Photos/clip.MP4"));
        assert!(already_compressed("x.jpeg"));
        assert!(!already_compressed("notes.txt"));
        assert!(!already_compressed(".zip"));
        assert!(!already_compressed("README"));
    }
}
