//! Serial background jobs and repository invalidation generations.
//!
//! A job is I/O, so this lives in `gitten-app`, never `core`. The runner knows
//! nothing about git or a UI: it executes extension and built-in jobs through
//! the same object-safe seam and reports lifecycle events to whichever client
//! owns it.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Work that may block and therefore must not run on a client's render thread.
pub trait Job: Send + 'static {
    fn name(&self) -> &str;

    /// What a clean finish announces, when announcing earns its sentence.
    /// `None` — every write's default — says nothing: most writes show
    /// themselves in the pane they changed. A job whose effect lands where
    /// the eye is not (a push moves counts on an upstream) overrides this,
    /// and a client says the sentence instead of leaving the reader to guess
    /// whether anything happened.
    fn confirmation(&self) -> Option<String> {
        None
    }

    fn run(self: Box<Self>) -> Result<(), String>;
}

enum Message {
    Run(Box<dyn Job>),
    Stop,
}

/// The cloneable extension-facing end of a [`Runner`].
#[derive(Clone)]
pub struct Submitter(Arc<Mutex<Option<Sender<Message>>>>);

impl Submitter {
    /// Queues a job without waiting for it to execute.
    pub fn submit(&self, job: Box<dyn Job>) -> Result<(), Box<dyn Job>> {
        let commands = self.0.lock().unwrap();
        let Some(commands) = commands.as_ref() else {
            return Err(job);
        };
        commands.send(Message::Run(job)).map_err(|e| match e.0 {
            Message::Run(job) => job,
            Message::Stop => unreachable!("submit never sends stop"),
        })
    }
}

/// The repository state produced by successful jobs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation(u64);

impl Generation {
    pub fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A worker lifecycle event. Only a successful finish carries a new generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Started {
        name: String,
    },
    Finished {
        name: String,
        outcome: Result<Generation, String>,
        /// The job's own [`confirmation`](Job::confirmation), read off it
        /// before it ran and forwarded verbatim. A client decides what to do
        /// with one — shown only beside a success is the usual answer.
        done: Option<String>,
    },
}

/// One FIFO worker and the sole receiver for its events.
pub struct Runner {
    commands: Arc<Mutex<Option<Sender<Message>>>>,
    events: Receiver<Event>,
}

impl Runner {
    pub fn new() -> Self {
        let (commands, work) = mpsc::channel();
        let (reports, events) = mpsc::channel();
        std::thread::Builder::new()
            .name("gitten-jobs".into())
            .spawn(move || worker(work, reports))
            .expect("failed to start job worker");
        Self {
            commands: Arc::new(Mutex::new(Some(commands))),
            events,
        }
    }

    pub fn submitter(&self) -> Submitter {
        Submitter(Arc::clone(&self.commands))
    }

    /// One non-blocking event, for a client to drain on its own event loop.
    pub fn try_next(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        // Do not join here: closing a window must never wait for a Git process.
        // Taking the sender also makes every surviving Submitter reject new
        // work atomically with queuing this stop behind already accepted jobs.
        if let Some(commands) = self.commands.lock().unwrap().take() {
            let _ = commands.send(Message::Stop);
        }
    }
}

fn worker(commands: Receiver<Message>, events: Sender<Event>) {
    let mut generation = Generation::default();
    while let Ok(message) = commands.recv() {
        let Message::Run(job) = message else { break };
        let name = job.name().to_string();
        let done = job.confirmation();
        let _ = events.send(Event::Started { name: name.clone() });
        let outcome = match catch_unwind(AssertUnwindSafe(|| job.run())) {
            Ok(Ok(())) => {
                generation = generation.next();
                Ok(generation)
            }
            Ok(Err(error)) => Err(error),
            Err(payload) => Err(format!("job panicked: {}", panic_text(&payload))),
        };
        let _ = events.send(Event::Finished {
            name,
            outcome,
            done,
        });
    }
}

fn panic_text(payload: &Box<dyn Any + Send>) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::Duration;

    struct Fake {
        name: &'static str,
        result: Result<(), &'static str>,
        ran: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Job for Fake {
        fn name(&self) -> &str {
            self.name
        }

        fn run(self: Box<Self>) -> Result<(), String> {
            self.ran.lock().unwrap().push(self.name);
            self.result.map_err(str::to_string)
        }
    }

    fn receive(runner: &Runner, count: usize) -> Vec<Event> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while events.len() < count && std::time::Instant::now() < deadline {
            if let Some(event) = runner.try_next() {
                events.push(event);
            } else {
                std::thread::yield_now();
            }
        }
        assert_eq!(events.len(), count, "worker did not report in time");
        events
    }

    #[test]
    fn jobs_are_fifo_and_only_success_advances_the_generation() {
        let runner = Runner::new();
        let submit = runner.submitter();
        let ran = Arc::new(Mutex::new(Vec::new()));
        for (name, result) in [("one", Ok(())), ("bad", Err("no")), ("two", Ok(()))] {
            assert!(submit
                .submit(Box::new(Fake {
                    name,
                    result,
                    ran: ran.clone(),
                }))
                .is_ok());
        }
        let events = receive(&runner, 6);
        assert_eq!(*ran.lock().unwrap(), ["one", "bad", "two"]);
        assert!(matches!(
            events[1],
            Event::Finished {
                outcome: Ok(Generation(1)),
                ..
            }
        ));
        assert!(matches!(
            events[3],
            Event::Finished {
                outcome: Err(_),
                ..
            }
        ));
        assert!(matches!(
            events[5],
            Event::Finished {
                outcome: Ok(Generation(2)),
                ..
            }
        ));
    }

    #[test]
    fn a_confirmation_is_read_off_the_job_and_forwarded_verbatim() {
        // The sentence is the job's own words, carried even through a
        // failure — what a client does with one beside an error is the
        // client's call, not the worker's.
        struct Announcing;
        impl Job for Announcing {
            fn name(&self) -> &str {
                "announce"
            }
            fn confirmation(&self) -> Option<String> {
                Some("sent".into())
            }
            fn run(self: Box<Self>) -> Result<(), String> {
                Err("declined".into())
            }
        }
        let runner = Runner::new();
        assert!(runner.submitter().submit(Box::new(Announcing)).is_ok());
        assert_eq!(
            receive(&runner, 2).pop(),
            Some(Event::Finished {
                name: "announce".into(),
                outcome: Err("declined".into()),
                done: Some("sent".into()),
            })
        );
    }

    struct Gated {
        caller: std::thread::ThreadId,
        worker: Arc<Mutex<Option<std::thread::ThreadId>>>,
        gate: Arc<Barrier>,
    }

    impl Job for Gated {
        fn name(&self) -> &str {
            "gated"
        }

        fn run(self: Box<Self>) -> Result<(), String> {
            *self.worker.lock().unwrap() = Some(std::thread::current().id());
            assert_ne!(self.caller, std::thread::current().id());
            self.gate.wait();
            Ok(())
        }
    }

    #[test]
    fn submission_returns_before_background_work_finishes() {
        let runner = Runner::new();
        let gate = Arc::new(Barrier::new(2));
        let worker = Arc::new(Mutex::new(None));
        assert!(runner
            .submitter()
            .submit(Box::new(Gated {
                caller: std::thread::current().id(),
                worker: worker.clone(),
                gate: gate.clone(),
            }))
            .is_ok());
        gate.wait();
        let _ = receive(&runner, 2);
        assert!(worker.lock().unwrap().is_some());
    }

    struct Panic;

    impl Job for Panic {
        fn name(&self) -> &str {
            "panic"
        }

        fn run(self: Box<Self>) -> Result<(), String> {
            panic!("boom")
        }
    }

    #[test]
    fn a_panicking_job_does_not_kill_the_worker() {
        let runner = Runner::new();
        let submit = runner.submitter();
        let ran = Arc::new(Mutex::new(Vec::new()));
        assert!(submit.submit(Box::new(Panic)).is_ok());
        assert!(submit
            .submit(Box::new(Fake {
                name: "after",
                result: Ok(()),
                ran,
            }))
            .is_ok());
        let events = receive(&runner, 4);
        assert!(matches!(
            events[1],
            Event::Finished {
                outcome: Err(_),
                ..
            }
        ));
        assert!(matches!(
            events[3],
            Event::Finished {
                outcome: Ok(Generation(1)),
                ..
            }
        ));
    }

    #[test]
    fn runner_drop_does_not_wait_for_submitter_clones() {
        let runner = Runner::new();
        let submit = runner.submitter();
        drop(runner);
        assert!(submit.submit(Box::new(Panic)).is_err());
    }
}
