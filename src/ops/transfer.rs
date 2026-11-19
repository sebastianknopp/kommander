// Copying and moving files and whole folder trees.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// This is the engine only: it knows nothing about windows or dialogs. It
// runs on a worker thread with GIO's blocking calls (as Nautilus does), so a
// slow disk or network share can never freeze the window, and it talks to
// the user through a `Delegate`: progress goes out, and when a file of the
// same name is already there it asks what to do and waits for the answer.
//
// The rules that keep files safe:
// - A source is only ever removed (when moving) after it has arrived
//   completely, and a source folder only with a plain "remove empty folder",
//   so it can never take anything with it that was left behind.
// - An existing file is only replaced when the user said so, and a folder is
//   never replaced by a file or the other way round.
// - Symbolic links are copied as links, never followed.
// - A half-written file from a failed or cancelled copy is removed again,
//   unless it replaced an older file (GIO swaps those in at the very end).

use std::path::{Path, PathBuf};

use gio::glib;
use gio::prelude::*;

const ATTRIBUTES: &str =
    "standard::name,standard::display-name,standard::type,standard::size,time::modified";
const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

/// Progress is reported at most this often (microseconds). The tests want
/// to see every step.
const PROGRESS_INTERVAL: i64 = if cfg!(test) { 0 } else { 50_000 };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Copy,
    Move,
}

/// What to transfer: `sources` go into the folder `dest_dir`.
#[derive(Clone, Debug)]
pub struct Request {
    pub kind: Kind,
    pub sources: Vec<gio::File>,
    pub dest_dir: gio::File,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    /// Still counting what there is to do; the totals are not final yet.
    pub scanning: bool,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Name of the file being worked on.
    pub current: String,
}

/// One of the two files that want the same name.
#[derive(Clone, Debug)]
pub struct Side {
    pub file: gio::File,
    pub is_folder: bool,
    pub size: u64,
    /// Seconds since 1970; 0 if unknown.
    pub modified: u64,
}

/// A file or folder called `name` is already at the destination.
#[derive(Clone, Debug)]
pub struct Conflict {
    pub name: String,
    pub source: Side,
    pub existing: Side,
    /// False when a file meets a folder: those can only be skipped or kept
    /// side by side.
    pub can_replace: bool,
}

impl Conflict {
    /// Two folders: "replace" means merging the source into the existing one.
    pub fn is_merge(&self) -> bool {
        self.source.is_folder && self.existing.is_folder
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Replace the existing file, or merge into the existing folder.
    Replace,
    Skip,
    /// Keep the existing one and give the new one a numbered name.
    KeepBoth,
    /// Stop the whole operation.
    Cancel,
}

#[derive(Clone, Copy, Debug)]
pub struct Answer {
    pub resolution: Resolution,
    /// Use this answer for all later conflicts of the same kind (files, or
    /// folders) in this operation.
    pub apply_to_all: bool,
}

/// The engine's line to the user. Called on the worker thread.
pub trait Delegate {
    fn progress(&mut self, progress: &Progress);
    /// May block until the user has decided.
    fn conflict(&mut self, conflict: &Conflict) -> Answer;
}

/// How an operation ended. Counts are about the top-level items asked for.
#[derive(Debug)]
pub struct Summary {
    pub kind: Kind,
    /// Where the items that arrived completely are now.
    pub done: Vec<gio::File>,
    /// Items the user chose to skip.
    pub skipped: u32,
    /// Items that did not, or not completely, arrive because of errors.
    pub incomplete: u32,
    /// Every single file or folder that failed, with the reason.
    pub failed: Vec<(gio::File, glib::Error)>,
    pub cancelled: bool,
    /// Set when the operation had to give up (for example: disk full).
    pub stopped: Option<glib::Error>,
}

/// Carries out `request`. Blocks until done; cancel through `cancellable`.
pub fn run(
    request: &Request,
    cancellable: &gio::Cancellable,
    delegate: &mut dyn Delegate,
) -> Summary {
    let mut summary = Summary {
        kind: request.kind,
        done: Vec::new(),
        skipped: 0,
        incomplete: 0,
        failed: Vec::new(),
        cancelled: false,
        stopped: None,
    };
    let mut job = Job {
        kind: request.kind,
        cancellable,
        delegate,
        progress: Progress::default(),
        last_emit: 0,
        sticky_file: None,
        sticky_folder: None,
        rename_works: true,
        failed: Vec::new(),
    };
    match job.run(request, &mut summary) {
        Ok(()) => {}
        Err(Stop::Cancelled) => summary.cancelled = true,
        Err(Stop::Fatal(error)) => summary.stopped = Some(error),
    }
    job.emit(true);
    summary.failed = job.failed;
    summary
}

/// Why the whole operation ends early.
enum Stop {
    Cancelled,
    Fatal(glib::Error),
}

/// Where one source goes.
struct Target {
    dest: gio::File,
    /// Replace the file that is there.
    overwrite: bool,
    /// The folder is there already; fill it instead of creating it.
    merge: bool,
}

impl Target {
    fn new(dest: gio::File) -> Self {
        Self { dest, overwrite: false, merge: false }
    }
}

enum Decision {
    Go(Target),
    Skip,
    /// Could not be decided; the error is already recorded.
    Failed,
}

/// What became of one source and everything in it.
#[derive(Clone, Copy, Default)]
struct Outcome {
    /// Something in it failed.
    failed: bool,
    /// Something of the source is still where it was (failed or skipped), so
    /// a moved folder must stay.
    left_behind: bool,
}

impl Outcome {
    const FAILED: Outcome = Outcome { failed: true, left_behind: true };
}

struct Job<'a> {
    kind: Kind,
    cancellable: &'a gio::Cancellable,
    delegate: &'a mut dyn Delegate,
    progress: Progress,
    last_emit: i64,
    /// Answers given "for all": one for file conflicts, one for folder merges.
    sticky_file: Option<Resolution>,
    sticky_folder: Option<Resolution>,
    /// Moving by simply renaming is possible between these two places (same
    /// disk). Turned off the first time it turns out not to be.
    rename_works: bool,
    failed: Vec<(gio::File, glib::Error)>,
}

impl Job<'_> {
    fn run(&mut self, request: &Request, summary: &mut Summary) -> Result<(), Stop> {
        self.progress.scanning = true;
        self.emit(true);

        // First decide where every source goes, asking about name clashes
        // before any long work starts. A move on the same disk is a mere
        // rename and is done right here.
        let mut planned = Vec::new();
        for source in &request.sources {
            self.check()?;
            let info = match source.query_info(ATTRIBUTES, NOFOLLOW, Some(self.cancellable)) {
                Ok(info) => info,
                Err(error) => {
                    self.fail(source, error)?;
                    summary.incomplete += 1;
                    continue;
                }
            };
            match self.resolve(source, &info, &request.dest_dir, &info.name())? {
                Decision::Skip => summary.skipped += 1,
                Decision::Failed => summary.incomplete += 1,
                Decision::Go(target) => {
                    if self.kind == Kind::Move && !target.merge {
                        match self.rename(source, &target) {
                            Ok(true) => {
                                summary.done.push(target.dest);
                                continue;
                            }
                            Ok(false) => {}
                            Err(error) => {
                                self.fail(source, error)?;
                                summary.incomplete += 1;
                                continue;
                            }
                        }
                    }
                    planned.push((source.clone(), info, target));
                }
            }
        }

        // Count what the rest amounts to, for the progress bar.
        for (source, info, _) in &planned {
            let (files, bytes) = self.measure(source, info)?;
            self.progress.files_total += files;
            self.progress.bytes_total += bytes;
        }
        self.progress.scanning = false;
        self.emit(true);

        for (source, info, target) in planned {
            self.check()?;
            let dest = target.dest.clone();
            if self.transfer(&source, &info, target, false)?.failed {
                summary.incomplete += 1;
            } else {
                summary.done.push(dest);
            }
        }
        Ok(())
    }

    /// Decides where `source` goes inside `dest_parent`, asking the user if
    /// something called `name` is already there.
    fn resolve(
        &mut self,
        source: &gio::File,
        info: &gio::FileInfo,
        dest_parent: &gio::File,
        name: &Path,
    ) -> Result<Decision, Stop> {
        let is_folder = is_folder(info);
        let dest = dest_parent.child(name);

        // Onto itself: copying makes a duplicate, moving has nothing to do.
        if source.equal(&dest) {
            return Ok(match self.kind {
                Kind::Move => Decision::Skip,
                Kind::Copy => Decision::Go(Target::new(self.free_name(dest_parent, name, is_folder))),
            });
        }
        if is_folder && (dest_parent.equal(source) || dest_parent.has_prefix(source)) {
            let error = glib::Error::new(
                gio::IOErrorEnum::WouldRecurse,
                "A folder cannot be put inside itself",
            );
            self.fail(source, error)?;
            return Ok(Decision::Failed);
        }

        let existing = match dest.query_info(ATTRIBUTES, NOFOLLOW, Some(self.cancellable)) {
            Ok(existing) => existing,
            Err(error) if error.matches(gio::IOErrorEnum::NotFound) => {
                return Ok(Decision::Go(Target::new(dest)));
            }
            Err(error) => {
                self.fail(source, error)?;
                return Ok(Decision::Failed);
            }
        };

        let conflict = Conflict {
            name: info.display_name().to_string(),
            source: side(source, info),
            existing: side(&dest, &existing),
            can_replace: is_folder == self::is_folder(&existing),
        };
        let merge = conflict.is_merge();
        let sticky = match (conflict.can_replace, merge) {
            (false, _) => None,
            (true, true) => self.sticky_folder,
            (true, false) => self.sticky_file,
        };
        let resolution = match sticky {
            Some(resolution) => resolution,
            None => {
                let answer = self.delegate.conflict(&conflict);
                if answer.apply_to_all
                    && conflict.can_replace
                    && answer.resolution != Resolution::Cancel
                {
                    if merge {
                        self.sticky_folder = Some(answer.resolution);
                    } else {
                        self.sticky_file = Some(answer.resolution);
                    }
                }
                answer.resolution
            }
        };
        match resolution {
            Resolution::Cancel => Err(Stop::Cancelled),
            Resolution::Skip => Ok(Decision::Skip),
            Resolution::KeepBoth => {
                Ok(Decision::Go(Target::new(self.free_name(dest_parent, name, is_folder))))
            }
            // Never swap a file for a folder or the other way round.
            Resolution::Replace if !conflict.can_replace => Ok(Decision::Skip),
            Resolution::Replace => Ok(Decision::Go(Target { dest, overwrite: !merge, merge })),
        }
    }

    /// Tries to move `source` by renaming it, which only works on the same
    /// disk. Ok(false) means it has to be copied over instead.
    fn rename(&mut self, source: &gio::File, target: &Target) -> Result<bool, glib::Error> {
        let mut flags =
            gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE;
        if target.overwrite {
            flags |= gio::FileCopyFlags::OVERWRITE;
        }
        match source.move_(&target.dest, flags, Some(self.cancellable), None) {
            Ok(()) => Ok(true),
            Err(error)
                if error.matches(gio::IOErrorEnum::NotSupported)
                    || error.matches(gio::IOErrorEnum::WouldRecurse) =>
            {
                self.rename_works = false;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Number of files and their bytes in `file` (a folder: everything in it).
    fn measure(&mut self, file: &gio::File, info: &gio::FileInfo) -> Result<(u64, u64), Stop> {
        self.check()?;
        if !is_folder(info) {
            return Ok((1, size(info)));
        }
        self.emit(false);
        let children = match self.children(file) {
            Ok(children) => children,
            Err(error) if error.matches(gio::IOErrorEnum::Cancelled) => {
                return Err(Stop::Cancelled);
            }
            // Unreadable folders show up as errors when their turn comes.
            Err(_) => return Ok((0, 0)),
        };
        let mut total = (0, 0);
        for child in children {
            let (files, bytes) = self.measure(&file.child(child.name()), &child)?;
            total.0 += files;
            total.1 += bytes;
        }
        Ok(total)
    }

    /// Counts `file` (and everything in it) as dealt with, so that the
    /// progress still reaches the end when something is skipped or fails.
    fn pass_over(&mut self, file: &gio::File, info: &gio::FileInfo) -> Result<(), Stop> {
        let (files, bytes) = self.measure(file, info)?;
        self.progress.files_done += files;
        self.progress.bytes_done += bytes;
        self.emit(false);
        Ok(())
    }

    /// Copies or moves `source` (with everything in it) to `target`.
    /// `nested` is true below the top level, where a move by renaming has not
    /// been tried yet.
    fn transfer(
        &mut self,
        source: &gio::File,
        info: &gio::FileInfo,
        target: Target,
        nested: bool,
    ) -> Result<Outcome, Stop> {
        self.check()?;
        self.progress.current = info.display_name().to_string();
        if !is_folder(info) {
            return self.transfer_file(source, info, &target);
        }

        // When merging folders on the same disk, the subfolders that are not
        // at the destination yet can still be moved over by renaming.
        if nested && self.kind == Kind::Move && !target.merge && self.rename_works {
            let (files, bytes) = self.measure(source, info)?;
            match self.rename(source, &target) {
                Ok(true) => {
                    self.progress.files_done += files;
                    self.progress.bytes_done += bytes;
                    self.emit(false);
                    return Ok(Outcome::default());
                }
                Ok(false) => {}
                Err(error) => {
                    self.fail(source, error)?;
                    self.progress.files_done += files;
                    self.progress.bytes_done += bytes;
                    return Ok(Outcome::FAILED);
                }
            }
        }

        if !target.merge
            && let Err(error) = target.dest.make_directory(Some(self.cancellable))
        {
            self.fail(source, error)?;
            self.pass_over(source, info)?;
            return Ok(Outcome::FAILED);
        }
        // The whole listing first: when moving, entries disappear as we go.
        let children = match self.children(source) {
            Ok(children) => children,
            Err(error) => {
                self.fail(source, error)?;
                return Ok(Outcome::FAILED);
            }
        };
        let mut outcome = Outcome::default();
        for child in children {
            self.check()?;
            let child_file = source.child(child.name());
            match self.resolve(&child_file, &child, &target.dest, &child.name())? {
                Decision::Skip => {
                    outcome.left_behind = true;
                    self.pass_over(&child_file, &child)?;
                }
                Decision::Failed => {
                    outcome = Outcome::FAILED;
                    self.pass_over(&child_file, &child)?;
                }
                Decision::Go(child_target) => {
                    let result = self.transfer(&child_file, &child, child_target, true)?;
                    outcome.failed |= result.failed;
                    outcome.left_behind |= result.left_behind;
                }
            }
        }
        // Permissions and dates last, so a read-only folder can be filled.
        if !target.merge {
            let _ = source.copy_attributes(
                &target.dest,
                gio::FileCopyFlags::NOFOLLOW_SYMLINKS,
                Some(self.cancellable),
            );
        }
        // Only removes the folder if it is empty by now.
        if self.kind == Kind::Move
            && !outcome.left_behind
            && let Err(error) = source.delete(Some(self.cancellable))
        {
            self.fail(source, error)?;
            outcome = Outcome::FAILED;
        }
        Ok(outcome)
    }

    fn transfer_file(
        &mut self,
        source: &gio::File,
        info: &gio::FileInfo,
        target: &Target,
    ) -> Result<Outcome, Stop> {
        let size = size(info);
        let mut flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS;
        if target.overwrite {
            flags |= gio::FileCopyFlags::OVERWRITE;
        }
        let kind = self.kind;
        let cancellable = self.cancellable;
        let before = self.progress.bytes_done;
        let result = {
            let mut on_progress = |current: i64, _total: i64| {
                self.progress.bytes_done = before + (current.max(0) as u64).min(size);
                self.emit(false);
            };
            match kind {
                Kind::Copy => {
                    source.copy(&target.dest, flags, Some(cancellable), Some(&mut on_progress))
                }
                // GIO renames where it can and otherwise copies, then
                // removes the source.
                Kind::Move => {
                    source.move_(&target.dest, flags, Some(cancellable), Some(&mut on_progress))
                }
            }
        };
        // Done or failed, this file's share of the work is behind us.
        self.progress.bytes_done = before + size;
        self.progress.files_done += 1;
        self.emit(false);

        let Err(error) = result else {
            return Ok(Outcome::default());
        };
        // Remove what was half written. Not when replacing (the old file is
        // only swapped out at the very end), and not when the name turned out
        // to be taken after all: then it is somebody else's file.
        if !target.overwrite && !error.matches(gio::IOErrorEnum::Exists) {
            let _ = target.dest.delete(gio::Cancellable::NONE);
        }
        self.fail(source, error)?;
        Ok(Outcome::FAILED)
    }

    fn children(&self, folder: &gio::File) -> Result<Vec<gio::FileInfo>, glib::Error> {
        let entries = folder.enumerate_children(ATTRIBUTES, NOFOLLOW, Some(self.cancellable))?;
        let mut children = Vec::new();
        while let Some(child) = entries.next_file(Some(self.cancellable))? {
            children.push(child);
        }
        let _ = entries.close(gio::Cancellable::NONE);
        Ok(children)
    }

    /// The first numbered variant of `name` that is not taken in `folder`.
    fn free_name(&self, folder: &gio::File, name: &Path, is_folder: bool) -> gio::File {
        let mut number = 2;
        loop {
            let candidate = folder.child(numbered(name, number, is_folder));
            if !candidate.query_exists(Some(self.cancellable)) {
                return candidate;
            }
            number += 1;
        }
    }

    /// Records that `file` failed. Errors that end the whole operation
    /// (cancelled, disk full) come back as `Stop` instead.
    fn fail(&mut self, file: &gio::File, error: glib::Error) -> Result<(), Stop> {
        if error.matches(gio::IOErrorEnum::Cancelled) {
            return Err(Stop::Cancelled);
        }
        if error.matches(gio::IOErrorEnum::NoSpace) {
            return Err(Stop::Fatal(error));
        }
        self.failed.push((file.clone(), error));
        Ok(())
    }

    fn check(&self) -> Result<(), Stop> {
        if self.cancellable.is_cancelled() { Err(Stop::Cancelled) } else { Ok(()) }
    }

    fn emit(&mut self, force: bool) {
        let now = glib::monotonic_time();
        if force || now - self.last_emit >= PROGRESS_INTERVAL {
            self.last_emit = now;
            self.delegate.progress(&self.progress);
        }
    }
}

/// A real folder. A symbolic link to a folder is not: it is copied as a link.
fn is_folder(info: &gio::FileInfo) -> bool {
    info.file_type() == gio::FileType::Directory
}

fn size(info: &gio::FileInfo) -> u64 {
    info.size().max(0) as u64
}

fn side(file: &gio::File, info: &gio::FileInfo) -> Side {
    Side {
        file: file.clone(),
        is_folder: is_folder(info),
        size: size(info),
        modified: info.attribute_uint64(gio::FILE_ATTRIBUTE_TIME_MODIFIED),
    }
}

/// "report.pdf" → "report (2).pdf"; folders and names without an extension
/// get the number at the end.
fn numbered(name: &Path, number: u32, is_folder: bool) -> PathBuf {
    let Some(text) = name.to_str() else {
        // Not valid text: leave the bytes alone and append.
        let mut raw = name.as_os_str().to_owned();
        raw.push(format!(" ({number})"));
        return raw.into();
    };
    let split = if is_folder { text.len() } else { extension_start(text) };
    format!("{} ({number}){}", &text[..split], &text[split..]).into()
}

/// Byte position where the extension of `name` starts (its dot), or the end
/// if it has none. A leading dot starts no extension (".bashrc"), and
/// compressed archives keep their inner one ("backup.tar.gz" → ".tar.gz").
fn extension_start(name: &str) -> usize {
    let first = name.chars().next().map_or(0, char::len_utf8);
    let Some(mut dot) = name[first..].rfind('.').map(|i| i + first) else {
        return name.len();
    };
    if dot + 1 == name.len() {
        return name.len();
    }
    if [".gz", ".bz2", ".sit", ".Z", ".bz", ".xz"].contains(&&name[dot..])
        && let Some(inner) = name[first..dot].rfind('.').map(|i| i + first)
        && inner > first
    {
        dot = inner;
    }
    dot
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    /// A fresh folder for one test, removed again when dropped.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Self {
            Self::under(&std::env::temp_dir(), name)
        }

        fn under(base: &Path, name: &str) -> Self {
            let path = base.join(format!("kommander-transfer-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }

        /// Creates the file `relative` (and its folders) with `content`.
        fn write(&self, relative: &str, content: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn mkdir(&self, relative: &str) {
            fs::create_dir_all(self.path(relative)).unwrap();
        }

        fn read(&self, relative: &str) -> String {
            fs::read_to_string(self.path(relative)).unwrap()
        }

        fn exists(&self, relative: &str) -> bool {
            self.path(relative).symlink_metadata().is_ok()
        }

        fn file(&self, relative: &str) -> gio::File {
            gio::File::for_path(self.path(relative))
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            // Folders made read-only by a test must be writable to go away.
            let _ = std::process::Command::new("chmod").arg("-R").arg("u+rwx").arg(&self.0).status();
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Answers conflicts from a prepared list and remembers what was asked.
    #[derive(Default)]
    struct Script {
        answers: VecDeque<Answer>,
        asked: Vec<String>,
        last: Progress,
        reports: u32,
        /// Cancel as soon as this many bytes are reported done.
        cancel_at: Option<(u64, gio::Cancellable)>,
    }

    impl Script {
        fn answering(answers: &[(Resolution, bool)]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|&(resolution, apply_to_all)| Answer { resolution, apply_to_all })
                    .collect(),
                ..Self::default()
            }
        }
    }

    impl Delegate for Script {
        fn progress(&mut self, progress: &Progress) {
            self.last = progress.clone();
            self.reports += 1;
            if let Some((bytes, cancellable)) = &self.cancel_at
                && progress.bytes_done >= *bytes
            {
                cancellable.cancel();
            }
        }

        fn conflict(&mut self, conflict: &Conflict) -> Answer {
            self.asked.push(conflict.name.clone());
            self.answers.pop_front().expect("a conflict the test did not expect")
        }
    }

    fn go(kind: Kind, sources: &[gio::File], dest_dir: gio::File, script: &mut Script) -> Summary {
        let request = Request { kind, sources: sources.to_vec(), dest_dir };
        run(&request, &gio::Cancellable::new(), script)
    }

    #[test]
    fn numbered_names() {
        let n = |name: &str, folder| numbered(Path::new(name), 2, folder).to_str().unwrap().to_owned();
        assert_eq!(n("report.pdf", false), "report (2).pdf");
        assert_eq!(n("backup.tar.gz", false), "backup (2).tar.gz");
        assert_eq!(n("README", false), "README (2)");
        assert_eq!(n(".bashrc", false), ".bashrc (2)");
        assert_eq!(n("trailing.", false), "trailing. (2)");
        assert_eq!(n("My.Folder", true), "My.Folder (2)");
        assert_eq!(n("über.größe.txt", false), "über.größe (2).txt");
    }

    #[test]
    fn copies_a_tree_and_reports_progress() {
        let t = Temp::new("copy-tree");
        t.write("src/tree/a.txt", "aaaa");
        t.write("src/tree/sub/deeper/b.txt", "bb");
        t.mkdir("src/tree/empty");
        t.write("src/single.txt", "s");
        t.mkdir("dst");

        let mut script = Script::default();
        let summary = go(
            Kind::Copy,
            &[t.file("src/tree"), t.file("src/single.txt")],
            t.file("dst"),
            &mut script,
        );

        assert_eq!(summary.done.len(), 2);
        assert!(summary.failed.is_empty() && !summary.cancelled);
        assert_eq!(t.read("dst/tree/a.txt"), "aaaa");
        assert_eq!(t.read("dst/tree/sub/deeper/b.txt"), "bb");
        assert!(t.path("dst/tree/empty").is_dir());
        assert_eq!(t.read("dst/single.txt"), "s");
        // The sources are untouched.
        assert_eq!(t.read("src/tree/a.txt"), "aaaa");
        // Progress ends complete: 3 files, 7 bytes.
        assert_eq!(script.last.files_total, 3);
        assert_eq!(script.last.files_done, 3);
        assert_eq!(script.last.bytes_total, 7);
        assert_eq!(script.last.bytes_done, 7);
        assert!(!script.last.scanning);
        assert!(script.asked.is_empty());
    }

    #[test]
    fn file_conflicts_replace_skip_keep_both() {
        let t = Temp::new("conflicts");
        for name in ["one.txt", "two.txt", "three.txt"] {
            t.write(&format!("src/{name}"), "new");
            t.write(&format!("dst/{name}"), "old");
        }
        let mut script = Script::answering(&[
            (Resolution::Replace, false),
            (Resolution::Skip, false),
            (Resolution::KeepBoth, false),
        ]);
        let summary = go(
            Kind::Copy,
            &[t.file("src/one.txt"), t.file("src/two.txt"), t.file("src/three.txt")],
            t.file("dst"),
            &mut script,
        );

        assert_eq!(script.asked, ["one.txt", "two.txt", "three.txt"]);
        assert_eq!(t.read("dst/one.txt"), "new");
        assert_eq!(t.read("dst/two.txt"), "old");
        assert_eq!(t.read("dst/three.txt"), "old");
        assert_eq!(t.read("dst/three (2).txt"), "new");
        assert_eq!(summary.done.len(), 2);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.incomplete, 0);
    }

    #[test]
    fn apply_to_all_asks_once_per_kind() {
        let t = Temp::new("apply-all");
        for name in ["a.txt", "b.txt", "c.txt"] {
            t.write(&format!("src/{name}"), "new");
            t.write(&format!("dst/{name}"), "old");
        }
        let mut script = Script::answering(&[(Resolution::Replace, true)]);
        let summary = go(
            Kind::Copy,
            &[t.file("src/a.txt"), t.file("src/b.txt"), t.file("src/c.txt")],
            t.file("dst"),
            &mut script,
        );
        assert_eq!(script.asked, ["a.txt"]);
        assert_eq!(summary.done.len(), 3);
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert_eq!(t.read(&format!("dst/{name}")), "new");
        }
    }

    #[test]
    fn merges_folders_and_asks_about_files_inside() {
        let t = Temp::new("merge");
        t.write("src/album/new.txt", "new");
        t.write("src/album/same.txt", "from source");
        t.write("src/album/sub/x.txt", "x");
        t.write("dst/album/same.txt", "at destination");
        t.write("dst/album/only-here.txt", "stays");

        // Merge the folder, then skip the one file that clashes inside.
        let mut script = Script::answering(&[(Resolution::Replace, false), (Resolution::Skip, false)]);
        let summary = go(Kind::Copy, &[t.file("src/album")], t.file("dst"), &mut script);

        assert_eq!(script.asked, ["album", "same.txt"]);
        assert_eq!(t.read("dst/album/new.txt"), "new");
        assert_eq!(t.read("dst/album/same.txt"), "at destination");
        assert_eq!(t.read("dst/album/only-here.txt"), "stays");
        assert_eq!(t.read("dst/album/sub/x.txt"), "x");
        // A skip inside is the user's choice, not a failure.
        assert_eq!(summary.done.len(), 1);
        assert_eq!(summary.incomplete, 0);
    }

    #[test]
    fn a_file_never_replaces_a_folder_or_the_reverse() {
        let t = Temp::new("mixed");
        t.write("src/thing", "a file");
        t.write("dst/thing/inside.txt", "precious");
        t.write("src2/other/inside.txt", "a folder");
        t.write("dst/other", "a file");

        // Even if told to replace, nothing is replaced.
        let mut script = Script::answering(&[(Resolution::Replace, true), (Resolution::KeepBoth, false)]);
        let summary = go(
            Kind::Copy,
            &[t.file("src/thing"), t.file("src2/other")],
            t.file("dst"),
            &mut script,
        );
        // "For all" does not carry over from such a clash: both were asked.
        assert_eq!(script.asked, ["thing", "other"]);
        assert_eq!(t.read("dst/thing/inside.txt"), "precious");
        assert_eq!(t.read("dst/other"), "a file");
        assert_eq!(t.read("dst/other (2)/inside.txt"), "a folder");
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.done.len(), 1);
    }

    #[test]
    fn copy_onto_itself_duplicates_and_move_does_nothing() {
        let t = Temp::new("same-folder");
        t.write("dir/note.txt", "n");
        t.write("dir/folder/x.txt", "x");

        let mut script = Script::default();
        let sources = [t.file("dir/note.txt"), t.file("dir/folder")];
        let summary = go(Kind::Copy, &sources, t.file("dir"), &mut script);
        assert_eq!(summary.done.len(), 2);
        assert_eq!(t.read("dir/note (2).txt"), "n");
        assert_eq!(t.read("dir/folder (2)/x.txt"), "x");
        // A second time takes the next number.
        go(Kind::Copy, &sources[..1], t.file("dir"), &mut script);
        assert_eq!(t.read("dir/note (3).txt"), "n");

        let summary = go(Kind::Move, &sources, t.file("dir"), &mut script);
        assert_eq!(summary.skipped, 2);
        assert_eq!(t.read("dir/note.txt"), "n");
        assert_eq!(t.read("dir/folder/x.txt"), "x");
        assert!(script.asked.is_empty());
    }

    #[test]
    fn a_folder_cannot_go_inside_itself() {
        let t = Temp::new("into-itself");
        t.write("a/b/c.txt", "c");
        let mut script = Script::default();
        for kind in [Kind::Copy, Kind::Move] {
            for dest in ["a", "a/b"] {
                let summary = go(kind, &[t.file("a")], t.file(dest), &mut script);
                assert_eq!(summary.incomplete, 1, "{kind:?} into {dest}");
                assert!(summary.failed[0].1.matches(gio::IOErrorEnum::WouldRecurse));
            }
        }
        assert_eq!(t.read("a/b/c.txt"), "c");
        assert!(!t.exists("a/a") && !t.exists("a/b/a"));
    }

    #[test]
    fn moves_on_the_same_disk() {
        let t = Temp::new("move");
        t.write("src/tree/a.txt", "a");
        t.write("src/tree/sub/b.txt", "b");
        t.write("src/file.txt", "f");
        t.mkdir("dst");

        let mut script = Script::default();
        let summary =
            go(Kind::Move, &[t.file("src/tree"), t.file("src/file.txt")], t.file("dst"), &mut script);
        assert_eq!(summary.done.len(), 2);
        assert!(summary.failed.is_empty());
        assert_eq!(t.read("dst/tree/sub/b.txt"), "b");
        assert_eq!(t.read("dst/file.txt"), "f");
        assert!(!t.exists("src/tree") && !t.exists("src/file.txt"));
    }

    #[test]
    fn move_merge_leaves_what_was_skipped() {
        let t = Temp::new("move-merge");
        t.write("src/album/new.txt", "new");
        t.write("src/album/same.txt", "from source");
        t.write("src/album/fresh/deep/y.txt", "y");
        t.write("src/album/both/z.txt", "z");
        t.write("dst/album/same.txt", "at destination");
        t.write("dst/album/both/old.txt", "old");

        // Merge "album", merge "both" inside it, skip "same.txt".
        let mut script = Script::answering(&[(Resolution::Replace, true), (Resolution::Skip, false)]);
        let summary = go(Kind::Move, &[t.file("src/album")], t.file("dst"), &mut script);

        assert_eq!(script.asked, ["album", "same.txt"]);
        assert_eq!(summary.done.len(), 1);
        assert_eq!(t.read("dst/album/new.txt"), "new");
        assert_eq!(t.read("dst/album/fresh/deep/y.txt"), "y");
        assert_eq!(t.read("dst/album/both/z.txt"), "z");
        assert_eq!(t.read("dst/album/both/old.txt"), "old");
        assert_eq!(t.read("dst/album/same.txt"), "at destination");
        // The skipped file is still at the source, and so is its folder;
        // everything that moved is gone from there.
        assert_eq!(t.read("src/album/same.txt"), "from source");
        assert!(!t.exists("src/album/new.txt"));
        assert!(!t.exists("src/album/fresh"));
        assert!(!t.exists("src/album/both"));
        // The progress still adds up, including the renamed subfolder.
        assert_eq!(script.last.files_done, script.last.files_total);
        assert_eq!(script.last.bytes_done, script.last.bytes_total);
    }

    #[test]
    fn move_replace_overwrites_the_existing_file() {
        let t = Temp::new("move-replace");
        t.write("src/a.txt", "new");
        t.write("dst/a.txt", "old");
        let mut script = Script::answering(&[(Resolution::Replace, false)]);
        let summary = go(Kind::Move, &[t.file("src/a.txt")], t.file("dst"), &mut script);
        assert_eq!(summary.done.len(), 1);
        assert_eq!(t.read("dst/a.txt"), "new");
        assert!(!t.exists("src/a.txt"));
    }

    #[test]
    fn moves_across_disks_by_copying() {
        // /dev/shm and the temporary folder are different file systems on
        // most Linux machines; without that there is nothing to test here.
        let shm = Path::new("/dev/shm");
        let different = shm.is_dir()
            && fs::metadata(shm).map(|m| m.dev()).ok()
                != fs::metadata(std::env::temp_dir()).map(|m| m.dev()).ok();
        if !different {
            eprintln!("skipped: no second file system available");
            return;
        }
        let here = Temp::new("cross-src");
        let there = Temp::under(shm, "cross-dst");
        here.write("tree/a.txt", "aaa");
        here.write("tree/sub/b.txt", "bb");
        here.mkdir("tree/empty");
        symlink("a.txt", here.path("tree/link")).unwrap();
        here.write("file.txt", "f");

        let mut script = Script::default();
        let summary =
            go(Kind::Move, &[here.file("tree"), here.file("file.txt")], there.file(""), &mut script);

        assert!(summary.failed.is_empty(), "{:?}", summary.failed);
        assert_eq!(summary.done.len(), 2);
        assert_eq!(there.read("tree/a.txt"), "aaa");
        assert_eq!(there.read("tree/sub/b.txt"), "bb");
        assert!(there.path("tree/empty").is_dir());
        assert!(there.path("tree/link").symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(there.read("file.txt"), "f");
        assert!(!here.exists("tree") && !here.exists("file.txt"));
        assert_eq!(script.last.files_done, script.last.files_total);
        assert_eq!(script.last.bytes_done, script.last.bytes_total);
    }

    #[test]
    fn symbolic_links_are_copied_as_links() {
        let t = Temp::new("links");
        t.write("outside/secret.txt", "secret");
        t.mkdir("src/tree");
        symlink(t.path("outside"), t.path("src/tree/to-folder")).unwrap();
        symlink("missing", t.path("src/tree/dangling")).unwrap();
        t.mkdir("dst");

        let mut script = Script::default();
        let summary = go(Kind::Copy, &[t.file("src/tree")], t.file("dst"), &mut script);
        assert!(summary.failed.is_empty(), "{:?}", summary.failed);
        let link = t.path("dst/tree/to-folder");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(link).unwrap(), t.path("outside"));
        assert!(t.path("dst/tree/dangling").symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(t.read("outside/secret.txt"), "secret");
    }

    #[test]
    fn errors_are_collected_and_the_rest_goes_on() {
        let t = Temp::new("errors");
        t.write("src/tree/ok.txt", "ok");
        t.write("src/tree/locked/hidden.txt", "h");
        t.write("src/after.txt", "after");
        t.mkdir("dst");
        fs::set_permissions(t.path("src/tree/locked"), fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(t.path("src/tree/locked")).is_ok() {
            eprintln!("skipped: permissions are not enforced (running as root?)");
            return;
        }

        let mut script = Script::default();
        let summary = go(
            Kind::Move,
            &[t.file("src/tree"), t.file("src/after.txt"), t.file("src/missing.txt")],
            t.file("dst"),
            &mut script,
        );
        // On the same disk "tree" is renamed as a whole, the locked folder
        // with it; only the missing file fails.
        assert_eq!(summary.done.len(), 2);
        assert_eq!(summary.incomplete, 1);
        assert!(summary.failed[0].1.matches(gio::IOErrorEnum::NotFound));

        // Copying has to read the locked folder, and reports it.
        let summary = go(Kind::Copy, &[t.file("dst/tree")], t.file("src"), &mut script);
        assert_eq!(summary.incomplete, 1);
        assert_eq!(summary.failed.len(), 1);
        assert!(summary.failed[0].1.matches(gio::IOErrorEnum::PermissionDenied));
        assert_eq!(t.read("src/tree/ok.txt"), "ok");
        assert!(!summary.cancelled && summary.stopped.is_none());
    }

    #[test]
    fn a_read_only_destination_fails_cleanly() {
        let t = Temp::new("read-only");
        t.write("src/a.txt", "a");
        t.write("src/b.txt", "b");
        t.mkdir("dst");
        fs::set_permissions(t.path("dst"), fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(t.path("dst/probe"), "").is_ok() {
            eprintln!("skipped: permissions are not enforced (running as root?)");
            return;
        }
        let mut script = Script::default();
        let summary =
            go(Kind::Move, &[t.file("src/a.txt"), t.file("src/b.txt")], t.file("dst"), &mut script);
        assert_eq!(summary.incomplete, 2);
        assert!(summary.done.is_empty());
        assert!(summary.failed.iter().all(|(_, e)| e.matches(gio::IOErrorEnum::PermissionDenied)));
        // Nothing was lost at the source.
        assert_eq!(t.read("src/a.txt"), "a");
        assert_eq!(t.read("src/b.txt"), "b");
    }

    #[test]
    fn cancelling_stops_and_leaves_no_half_file() {
        let t = Temp::new("cancel");
        t.write("src/tree/1.txt", "1111");
        t.write("src/tree/2.txt", "2222");
        t.write("src/tree/3.txt", "3333");
        t.mkdir("dst");

        let cancellable = gio::Cancellable::new();
        let mut script = Script { cancel_at: Some((4, cancellable.clone())), ..Script::default() };
        let request =
            Request { kind: Kind::Copy, sources: vec![t.file("src/tree")], dest_dir: t.file("dst") };
        let summary = run(&request, &cancellable, &mut script);

        assert!(summary.cancelled);
        assert!(summary.done.is_empty());
        // Whatever arrived is whole; nothing is half written.
        let arrived: Vec<_> = fs::read_dir(t.path("dst/tree"))
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        assert!(arrived.len() < 3);
        assert!(arrived.iter().all(|content| content.len() == 4));
        // The sources are all still there.
        for name in ["1.txt", "2.txt", "3.txt"] {
            assert!(t.exists(&format!("src/tree/{name}")));
        }
    }

    #[test]
    fn cancel_as_a_conflict_answer_stops_everything() {
        let t = Temp::new("cancel-answer");
        t.write("src/a.txt", "new");
        t.write("src/b.txt", "new");
        t.write("dst/a.txt", "old");
        let mut script = Script::answering(&[(Resolution::Cancel, true)]);
        let summary =
            go(Kind::Copy, &[t.file("src/a.txt"), t.file("src/b.txt")], t.file("dst"), &mut script);
        assert!(summary.cancelled);
        assert_eq!(t.read("dst/a.txt"), "old");
        assert!(!t.exists("dst/b.txt"));
    }
}
