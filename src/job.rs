/// A job as submitted by the user (immutable input).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub pid: usize,
    pub name: String,
    /// Time unit at which the job enters the ready queue.
    pub arrival: u32,
    /// Total CPU time required (must be > 0).
    pub burst: u32,
    /// Static priority. Lower number = higher priority (Unix `nice` convention).
    pub priority: u32,
}

/// Runtime state of a job while the simulation is running.
#[derive(Clone, Debug)]
pub struct Proc {
    pub job: Job,
    pub remaining: u32,
    /// Time of the first dispatch (used for response time).
    pub first_run: Option<u32>,
    /// Time at which the job finished.
    pub completion: Option<u32>,
}

impl Proc {
    pub fn new(job: Job) -> Self {
        let remaining = job.burst;
        Proc {
            job,
            remaining,
            first_run: None,
            completion: None,
        }
    }
}
