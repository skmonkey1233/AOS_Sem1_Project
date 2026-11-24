use crate::job::Proc;
use std::collections::VecDeque;

/// Every scheduling algorithm implements this trait. The engine calls it
/// once per simulated time unit, so preemption falls out naturally: a policy
/// is simply free to return a different process on the next call.
pub trait Policy {
    fn name(&self) -> String;
    /// A job has just arrived and entered the ready state.
    fn admit(&mut self, pid: usize, procs: &[Proc]);
    /// Choose the process that gets the CPU for the next time unit.
    /// Return `None` if nothing is ready (CPU idles).
    fn pick(&mut self, now: u32, procs: &[Proc]) -> Option<usize>;
    /// Called after the chosen process executed for one time unit.
    fn after_run(&mut self, pid: usize, finished: bool, procs: &[Proc]);
}

/// Tunable parameters shared by the algorithms.
#[derive(Clone, Debug)]
pub struct Config {
    /// Time quantum for Round Robin.
    pub quantum: u32,
    /// Quantum of each MLFQ level, top level first.
    pub mlfq_quanta: Vec<u32>,
    /// MLFQ priority boost period (anti-starvation). 0 disables it.
    pub boost: u32,
    /// Aging: priority improves by 1 for every `age_every` units waited.
    pub age_every: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            quantum: 4,
            mlfq_quanta: vec![2, 4, 8],
            boost: 50,
            age_every: 5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    Fcfs,
    Sjf,
    Srtf,
    Priority,
    PriorityPreemptive,
    PriorityAging,
    RoundRobin,
    Mlfq,
}

impl Algo {
    pub const ALL: [Algo; 8] = [
        Algo::Fcfs,
        Algo::Sjf,
        Algo::Srtf,
        Algo::Priority,
        Algo::PriorityPreemptive,
        Algo::PriorityAging,
        Algo::RoundRobin,
        Algo::Mlfq,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Algo::Fcfs => "fcfs",
            Algo::Sjf => "sjf",
            Algo::Srtf => "srtf",
            Algo::Priority => "priority",
            Algo::PriorityPreemptive => "priority-p",
            Algo::PriorityAging => "priority-aging",
            Algo::RoundRobin => "rr",
            Algo::Mlfq => "mlfq",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Algo::Fcfs => "FCFS",
            Algo::Sjf => "SJF",
            Algo::Srtf => "SRTF",
            Algo::Priority => "PRIO",
            Algo::PriorityPreemptive => "PRIO-P",
            Algo::PriorityAging => "PRIO-AGE",
            Algo::RoundRobin => "RR",
            Algo::Mlfq => "MLFQ",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Algo::Fcfs => "First-Come First-Served; non-preemptive, simple, suffers convoy effect",
            Algo::Sjf => "Shortest Job First; non-preemptive, optimal avg wait without preemption",
            Algo::Srtf => "Shortest Remaining Time First; preemptive SJF, optimal avg wait overall",
            Algo::Priority => "Static priority; non-preemptive (lower number = higher priority)",
            Algo::PriorityPreemptive => {
                "Static priority; a higher-priority arrival preempts the CPU"
            }
            Algo::PriorityAging => "Preemptive priority + aging so waiting jobs cannot starve",
            Algo::RoundRobin => "Time-sliced FIFO; fair and responsive, tunable with --quantum",
            Algo::Mlfq => {
                "Multi-Level Feedback Queue; learns job length, boosts to avoid starvation"
            }
        }
    }

    pub fn parse(s: &str) -> Option<Algo> {
        let s = s.to_ascii_lowercase();
        Algo::ALL.iter().copied().find(|a| a.id() == s)
    }

    pub fn build(self, cfg: &Config) -> Box<dyn Policy> {
        match self {
            Algo::Fcfs => key("FCFS", false, |p, _| (p.job.arrival as i64, 0)),
            Algo::Sjf => key("SJF (non-preemptive)", false, |p, _| {
                (p.job.burst as i64, p.job.arrival as i64)
            }),
            Algo::Srtf => key("SRTF (preemptive SJF)", true, |p, _| {
                (p.remaining as i64, p.job.arrival as i64)
            }),
            Algo::Priority => key("Priority (non-preemptive)", false, |p, _| {
                (p.job.priority as i64, p.job.arrival as i64)
            }),
            Algo::PriorityPreemptive => key("Priority (preemptive)", true, |p, _| {
                (p.job.priority as i64, p.job.arrival as i64)
            }),
            Algo::PriorityAging => {
                let a = cfg.age_every.max(1) as i64;
                key(
                    &format!("Priority + aging (every {})", a),
                    true,
                    move |p, now| {
                        // Time spent ready-but-not-running so far.
                        let waited =
                            now as i64 - p.job.arrival as i64 - (p.job.burst - p.remaining) as i64;
                        // Priority improves by one whole level per `a` units waited.
                        // Whole levels (not per-tick) keep the ordering stable, so
                        // aging does not degrade into a quantum-1 Round Robin.
                        (p.job.priority as i64 - waited / a, p.job.arrival as i64)
                    },
                )
            }
            Algo::RoundRobin => Box::new(RoundRobin::new(cfg.quantum.max(1))),
            Algo::Mlfq => Box::new(Mlfq::new(cfg.mlfq_quanta.clone(), cfg.boost)),
        }
    }
}

// ---------------------------------------------------------------------------
// Key based policies: FCFS, SJF, SRTF, Priority (all variants)
// ---------------------------------------------------------------------------

/// Sort key used by `KeyPolicy`: lower keys run first.
type KeyFn = Box<dyn Fn(&Proc, u32) -> (i64, i64)>;

/// Picks the ready process with the smallest key. When `preemptive` is false
/// the chosen process keeps the CPU until it finishes.
struct KeyPolicy {
    name: String,
    preemptive: bool,
    ready: Vec<usize>,
    running: Option<usize>,
    key: KeyFn,
}

fn key(
    name: &str,
    preemptive: bool,
    f: impl Fn(&Proc, u32) -> (i64, i64) + 'static,
) -> Box<dyn Policy> {
    Box::new(KeyPolicy {
        name: name.to_string(),
        preemptive,
        ready: Vec::new(),
        running: None,
        key: Box::new(f),
    })
}

impl Policy for KeyPolicy {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn admit(&mut self, pid: usize, _procs: &[Proc]) {
        self.ready.push(pid);
    }

    fn pick(&mut self, now: u32, procs: &[Proc]) -> Option<usize> {
        if !self.preemptive {
            if let Some(r) = self.running {
                return Some(r);
            }
        }
        let running = self.running;
        let best = self.ready.iter().copied().min_by_key(|&i| {
            let k = (self.key)(&procs[i], now);
            // Ties: keep the current process (avoids pointless switches), then lowest pid.
            (k.0, k.1, if Some(i) == running { 0 } else { 1 }, i)
        });
        self.running = best;
        best
    }

    fn after_run(&mut self, pid: usize, finished: bool, _procs: &[Proc]) {
        if finished {
            self.ready.retain(|&x| x != pid);
            if self.running == Some(pid) {
                self.running = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Round Robin
// ---------------------------------------------------------------------------

struct RoundRobin {
    quantum: u32,
    queue: VecDeque<usize>,
    running: Option<usize>,
    used: u32,
}

impl RoundRobin {
    fn new(quantum: u32) -> Self {
        RoundRobin {
            quantum,
            queue: VecDeque::new(),
            running: None,
            used: 0,
        }
    }
}

impl Policy for RoundRobin {
    fn name(&self) -> String {
        format!("Round Robin (q={})", self.quantum)
    }

    fn admit(&mut self, pid: usize, _procs: &[Proc]) {
        self.queue.push_back(pid);
    }

    fn pick(&mut self, _now: u32, _procs: &[Proc]) -> Option<usize> {
        if self.running.is_none() {
            self.running = self.queue.pop_front();
            self.used = 0;
        }
        self.running
    }

    fn after_run(&mut self, pid: usize, finished: bool, _procs: &[Proc]) {
        self.used += 1;
        if finished {
            self.running = None;
        } else if self.used >= self.quantum {
            // Engine admits same-instant arrivals before calling us, so a new
            // arrival correctly queues ahead of the pre-empted process.
            self.queue.push_back(pid);
            self.running = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Multi-Level Feedback Queue
// ---------------------------------------------------------------------------

/// Rules:
///  1. New jobs enter the top queue.
///  2. A job that uses its whole quantum is demoted one level.
///  3. A job in a lower queue is preempted when a higher queue becomes non-empty
///     (it goes to the back of its own queue with a fresh quantum).
///  4. Every `boost` time units all jobs move back to the top queue
///     (prevents starvation and lets changed behaviour be re-learned).
struct Mlfq {
    quanta: Vec<u32>,
    boost: u32,
    last_boost: u32,
    queues: Vec<VecDeque<usize>>,
    level: Vec<usize>,
    running: Option<usize>,
    used: u32,
}

impl Mlfq {
    fn new(mut quanta: Vec<u32>, boost: u32) -> Self {
        if quanta.is_empty() {
            quanta = vec![4];
        }
        for q in quanta.iter_mut() {
            *q = (*q).max(1);
        }
        let levels = quanta.len();
        Mlfq {
            quanta,
            boost,
            last_boost: 0,
            queues: vec![VecDeque::new(); levels],
            level: Vec::new(),
            running: None,
            used: 0,
        }
    }
}

impl Policy for Mlfq {
    fn name(&self) -> String {
        format!("MLFQ (quanta {:?}, boost {})", self.quanta, self.boost)
    }

    fn admit(&mut self, pid: usize, _procs: &[Proc]) {
        if self.level.len() <= pid {
            self.level.resize(pid + 1, 0);
        }
        self.level[pid] = 0;
        self.queues[0].push_back(pid);
    }

    fn pick(&mut self, now: u32, _procs: &[Proc]) -> Option<usize> {
        if self.boost > 0 && now > 0 && now.is_multiple_of(self.boost) && self.last_boost != now {
            self.last_boost = now;
            for lvl in 1..self.queues.len() {
                while let Some(p) = self.queues[lvl].pop_front() {
                    self.level[p] = 0;
                    self.queues[0].push_back(p);
                }
            }
            if let Some(r) = self.running {
                self.level[r] = 0;
                self.used = 0;
            }
        }

        let best = (0..self.queues.len()).find(|&l| !self.queues[l].is_empty());
        if let Some(r) = self.running {
            match best {
                Some(b) if b < self.level[r] => {
                    let l = self.level[r];
                    self.queues[l].push_back(r);
                    self.running = None;
                }
                _ => return Some(r),
            }
        }
        if self.running.is_none() {
            if let Some(b) = best {
                self.running = self.queues[b].pop_front();
                self.used = 0;
            }
        }
        self.running
    }

    fn after_run(&mut self, pid: usize, finished: bool, _procs: &[Proc]) {
        self.used += 1;
        if finished {
            self.running = None;
            return;
        }
        let l = self.level[pid];
        if self.used >= self.quanta[l] {
            let nl = (l + 1).min(self.quanta.len() - 1);
            self.level[pid] = nl;
            self.queues[nl].push_back(pid);
            self.running = None;
        }
    }
}
