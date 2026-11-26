// Runs copy and move operations (and packing and unpacking archives) on a
// worker thread, one after the other, and passes what they report back to
// the window.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The engine (ops::transfer) blocks while it works, so it gets a thread of
// its own; the window must never wait for a disk. Requests queue up: a second
// F5 while one copy is running waits its turn. Everything the engine reports
// (progress, a name clash, the end) travels to the main loop as an `Event`;
// widgets are only ever touched there. For a name clash the worker waits
// until the window has an answer; if the window is gone, the answer is
// Cancel, so the worker can never hang.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use futures_channel::mpsc as channel;
use futures_util::StreamExt;
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::ops::archive;
use crate::ops::transfer::{self, Answer, Conflict, Kind, Progress, Request, Resolution, Summary};

pub type JobId = u64;

/// What a job does: copy or move, or make or unpack an archive.
pub enum Task {
    Transfer(Request),
    Archive(archive::Request),
}

impl From<Request> for Task {
    fn from(request: Request) -> Self {
        Task::Transfer(request)
    }
}

impl From<archive::Request> for Task {
    fn from(request: archive::Request) -> Self {
        Task::Archive(request)
    }
}

/// How a job ended.
pub enum Outcome {
    /// `gone` are the sources of a move that are no longer where they were.
    Transfer { summary: Summary, gone: Vec<gio::File> },
    Archive(archive::Summary),
}

/// What the worker reports, in the order it happens.
pub enum Event {
    /// The job has left the queue and is being worked on.
    Started(JobId),
    Progress(JobId, Progress),
    /// A name clash. Answer through the sender; dropping it unanswered
    /// means Cancel.
    Conflict(JobId, Conflict, mpsc::Sender<Answer>),
    /// The job is over.
    Finished { id: JobId, outcome: Outcome },
}

/// One request on its way to the worker.
struct Work {
    id: JobId,
    task: Task,
    cancellable: gio::Cancellable,
}

/// The queue of operations. Lives on the main thread.
pub struct Jobs {
    queue: mpsc::Sender<Work>,
    /// Jobs that are queued or running, to cancel them.
    pending: Rc<RefCell<HashMap<JobId, gio::Cancellable>>>,
    next_id: Cell<JobId>,
}

impl Jobs {
    /// Starts the worker. `on_event` is called on the main loop for every
    /// event of every job.
    pub fn new(on_event: impl Fn(Event) + 'static) -> Self {
        let (queue, work) = mpsc::channel::<Work>();
        let (events, mut incoming) = channel::unbounded::<Event>();
        thread::Builder::new()
            .name("kommander-transfer".into())
            .spawn(move || worker(work, events))
            .expect("could not start the thread for copying and moving");

        let pending: Rc<RefCell<HashMap<JobId, gio::Cancellable>>> = Rc::default();
        let pending_inner = pending.clone();
        glib::spawn_future_local(async move {
            while let Some(event) = incoming.next().await {
                if let Event::Finished { id, .. } = &event {
                    pending_inner.borrow_mut().remove(id);
                }
                on_event(event);
            }
        });
        Self { queue, pending, next_id: Cell::new(1) }
    }

    /// Puts `task` in the queue and returns its job number.
    pub fn submit(&self, task: impl Into<Task>) -> JobId {
        let task = task.into();
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let cancellable = gio::Cancellable::new();
        self.pending.borrow_mut().insert(id, cancellable.clone());
        if self.queue.send(Work { id, task, cancellable }).is_err() {
            // Only if the worker thread died; there is nothing to run it.
            self.pending.borrow_mut().remove(&id);
        }
        id
    }

    /// Stops job `id` (or takes it out of the queue: it then ends at once,
    /// as cancelled).
    pub fn cancel(&self, id: JobId) {
        if let Some(cancellable) = self.pending.borrow().get(&id) {
            cancellable.cancel();
        }
    }

    pub fn cancel_all(&self) {
        for cancellable in self.pending.borrow().values() {
            cancellable.cancel();
        }
    }

    pub fn is_cancelled(&self, id: JobId) -> bool {
        self.pending.borrow().get(&id).is_none_or(|cancellable| cancellable.is_cancelled())
    }
}

impl Drop for Jobs {
    /// The window is going away: whatever is still queued must not start.
    fn drop(&mut self) {
        self.cancel_all();
    }
}

/// The worker thread: runs one request after the other until the queue's
/// sending side (the `Jobs`) is gone.
fn worker(work: mpsc::Receiver<Work>, events: channel::UnboundedSender<Event>) {
    for Work { id, task, cancellable } in work {
        // Nobody listens any more: nothing should start.
        if events.is_closed() {
            break;
        }
        let _ = events.unbounded_send(Event::Started(id));
        let mut delegate = Forward { id, events: events.clone() };
        let outcome = match task {
            Task::Transfer(request) => {
                let summary = transfer::run(&request, &cancellable, &mut delegate);
                let gone = match request.kind {
                    Kind::Move => request
                        .sources
                        .iter()
                        .filter(|source| !source.query_exists(gio::Cancellable::NONE))
                        .cloned()
                        .collect(),
                    Kind::Copy => Vec::new(),
                };
                Outcome::Transfer { summary, gone }
            }
            Task::Archive(request) => Outcome::Archive(archive::run(&request, &cancellable, &mut delegate)),
        };
        let _ = events.unbounded_send(Event::Finished { id, outcome });
    }
}

/// The engine's delegate on the worker thread: forwards everything to the
/// main loop.
struct Forward {
    id: JobId,
    events: channel::UnboundedSender<Event>,
}

impl transfer::Delegate for Forward {
    fn progress(&mut self, progress: &Progress) {
        let _ = self.events.unbounded_send(Event::Progress(self.id, progress.clone()));
    }

    /// Waits for the window's answer. No window, or no answer: Cancel.
    fn conflict(&mut self, conflict: &Conflict) -> Answer {
        let cancel = Answer { resolution: Resolution::Cancel, apply_to_all: false };
        let (reply, answer) = mpsc::channel();
        if self.events.unbounded_send(Event::Conflict(self.id, conflict.clone(), reply)).is_err() {
            return cancel;
        }
        answer.recv().unwrap_or(cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn temp_folder(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("kommander-jobs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// Runs the main context until `done` says so (or ten seconds pass).
    fn run_until(context: &glib::MainContext, done: impl Fn() -> bool) {
        let start = Instant::now();
        while !done() && start.elapsed() < Duration::from_secs(10) {
            context.iteration(false);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(done(), "timed out");
    }

    #[test]
    fn jobs_run_in_turn_and_conflicts_wait_for_an_answer() {
        let dir = temp_folder("queue");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("dst")).unwrap();
        fs::write(dir.join("src/a.txt"), "new").unwrap();
        fs::write(dir.join("dst/a.txt"), "old").unwrap();
        fs::write(dir.join("src/b.txt"), "b").unwrap();

        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let log: Rc<RefCell<Vec<String>>> = Rc::default();
                let log_inner = log.clone();
                let jobs = Jobs::new(move |event| {
                    let mut log = log_inner.borrow_mut();
                    match event {
                        Event::Started(id) => log.push(format!("start {id}")),
                        Event::Progress(..) => {}
                        Event::Conflict(id, conflict, reply) => {
                            log.push(format!("conflict {id} {}", conflict.name));
                            let answer = Answer { resolution: Resolution::Replace, apply_to_all: false };
                            reply.send(answer).unwrap();
                        }
                        Event::Finished { id, outcome: Outcome::Transfer { summary, gone } } => {
                            log.push(format!("end {id} {} {}", summary.done.len(), gone.len()))
                        }
                        Event::Finished { .. } => unreachable!("no archives here"),
                    }
                });
                let file = |p: &str| gio::File::for_path(dir.join(p));
                let first = jobs.submit(Request {
                    kind: Kind::Copy,
                    sources: vec![file("src/a.txt")],
                    dest_dir: file("dst"),
                });
                let second = jobs.submit(Request {
                    kind: Kind::Move,
                    sources: vec![file("src/b.txt")],
                    dest_dir: file("dst"),
                });
                run_until(&context, || log.borrow().len() == 5);
                assert_eq!(
                    *log.borrow(),
                    [
                        format!("start {first}"),
                        format!("conflict {first} a.txt"),
                        format!("end {first} 1 0"),
                        format!("start {second}"),
                        format!("end {second} 1 1"),
                    ]
                );
                assert!(jobs.is_cancelled(first), "finished jobs are no longer pending");
            })
            .unwrap();
        assert_eq!(fs::read_to_string(dir.join("dst/a.txt")).unwrap(), "new");
        assert!(dir.join("dst/b.txt").exists() && !dir.join("src/b.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unanswered_conflict_means_cancel() {
        let dir = temp_folder("unanswered");
        fs::create_dir_all(dir.join("dst")).unwrap();
        fs::write(dir.join("a.txt"), "new").unwrap();
        fs::write(dir.join("dst/a.txt"), "old").unwrap();

        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let cancelled = Rc::new(Cell::new(None));
                let cancelled_inner = cancelled.clone();
                // The reply sender is dropped without an answer, as when the
                // window closes while the question is open.
                let jobs = Jobs::new(move |event| {
                    if let Event::Finished { outcome: Outcome::Transfer { summary, .. }, .. } = event {
                        cancelled_inner.set(Some(summary.cancelled));
                    }
                });
                jobs.submit(Request {
                    kind: Kind::Copy,
                    sources: vec![gio::File::for_path(dir.join("a.txt"))],
                    dest_dir: gio::File::for_path(dir.join("dst")),
                });
                run_until(&context, || cancelled.get().is_some());
                assert_eq!(cancelled.get(), Some(true));
            })
            .unwrap();
        assert_eq!(fs::read_to_string(dir.join("dst/a.txt")).unwrap(), "old");
        let _ = fs::remove_dir_all(&dir);
    }
}
