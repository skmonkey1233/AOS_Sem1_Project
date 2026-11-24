use crate::job::{Job, Proc};
use crate::policy::Policy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Run(usize),
    Idle,
    /// Context-switch overhead.
    Switch,
}

/// A contiguous stretch of the timeline: `[start, end)`.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub kind: Kind,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug)]
pub struct SimResult {
    pub policy: String,
    pub procs: Vec<Proc>,
    pub timeline: Vec<Segment>,
    pub context_switches: u32,
    /// Time at which the last job finished.
    pub makespan: u32,
    /// Time units spent executing jobs.
    pub busy: u32,
    /// Time units lost to context-switch overhead.
    pub overhead: u32,
}

fn push(tl: &mut Vec<Segment>, kind: Kind, start: u32, end: u32) {
    if let Some(last) = tl.last_mut() {
        if last.kind == kind && last.end == start {
            last.end = end;
            return;
        }
    }
    tl.push(Segment { kind, start, end });
}

fn admit_arrivals(
    now: u32,
    order: &[usize],
    admitted: &mut [bool],
    dependencies: &[Option<usize>],
    procs: &[Proc],
    policy: &mut dyn Policy,
) {
    for &pid in order {
        if admitted[pid] || procs[pid].job.arrival > now {
            continue;
        }
        let unblocked = dependencies[pid].is_none_or(|parent| procs[parent].completion.is_some());
        if unblocked {
            policy.admit(pid, procs);
            admitted[pid] = true;
        }
    }
}

/// Run one full simulation.
///
/// `cs_cost` is the number of time units the CPU spends switching whenever it
/// is handed to a *different* process than the one that ran last.
pub fn simulate(jobs: &[Job], policy: &mut dyn Policy, cs_cost: u32) -> Result<SimResult, String> {
    simulate_with_dependencies(jobs, &vec![None; jobs.len()], policy, cs_cost)
}

/// Run a simulation with optional process prerequisites. A process is admitted
/// to the ready queue only after its arrival time and its prerequisite's
/// completion. Dependencies are zero-based process indices and must form a DAG.
pub fn simulate_with_dependencies(
    jobs: &[Job],
    dependencies: &[Option<usize>],
    policy: &mut dyn Policy,
    cs_cost: u32,
) -> Result<SimResult, String> {
    if jobs.is_empty() {
        return Err("workload is empty".into());
    }
    if dependencies.len() != jobs.len() {
        return Err("dependency list must have one entry per process".into());
    }
    if let Some(j) = jobs.iter().find(|j| j.burst == 0) {
        return Err(format!(
            "job {} has burst 0; burst must be at least 1",
            j.name
        ));
    }
    validate_dependencies(dependencies)?;

    let n = jobs.len();
    let mut procs: Vec<Proc> = jobs.iter().cloned().map(Proc::new).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (jobs[i].arrival, i));

    let mut timeline = Vec::new();
    let mut admitted = vec![false; n];
    let mut now = 0u32;
    let mut done = 0usize;
    let mut last: Option<usize> = None;
    let mut switches = 0u32;
    let mut busy = 0u32;
    let mut overhead = 0u32;

    admit_arrivals(now, &order, &mut admitted, dependencies, &procs, policy);

    while done < n {
        match policy.pick(now, &procs) {
            None => {
                // Nothing is ready: jump to the next process arrival. It may
                // remain blocked until its prerequisite completes.
                let next_arrival = order
                    .iter()
                    .copied()
                    .filter(|&pid| !admitted[pid])
                    .map(|pid| jobs[pid].arrival)
                    .filter(|&arrival| arrival > now)
                    .min();
                let t = match next_arrival {
                    Some(t) => t,
                    None => {
                        return Err(format!(
                            "policy '{}' stalled with work left; check process dependencies",
                            policy.name()
                        ))
                    }
                };
                push(&mut timeline, Kind::Idle, now, t);
                now = t;
                admit_arrivals(now, &order, &mut admitted, dependencies, &procs, policy);
            }
            Some(pid) => {
                if procs[pid].remaining == 0 {
                    return Err(format!("policy '{}' picked a finished job", policy.name()));
                }
                if let Some(l) = last {
                    if l != pid {
                        switches += 1;
                        for _ in 0..cs_cost {
                            push(&mut timeline, Kind::Switch, now, now + 1);
                            now += 1;
                            overhead += 1;
                            admit_arrivals(
                                now,
                                &order,
                                &mut admitted,
                                dependencies,
                                &procs,
                                policy,
                            );
                        }
                    }
                }
                last = Some(pid);

                if procs[pid].first_run.is_none() {
                    procs[pid].first_run = Some(now);
                }
                procs[pid].remaining -= 1;
                let finished = procs[pid].remaining == 0;
                push(&mut timeline, Kind::Run(pid), now, now + 1);
                busy += 1;
                now += 1;
                if finished {
                    procs[pid].completion = Some(now);
                    done += 1;
                }
                // Arrivals at the instant the tick ends are admitted *before*
                // the policy requeues the process that just ran.
                admit_arrivals(now, &order, &mut admitted, dependencies, &procs, policy);
                policy.after_run(pid, finished, &procs);
            }
        }
    }

    Ok(SimResult {
        policy: policy.name(),
        procs,
        timeline,
        context_switches: switches,
        makespan: now,
        busy,
        overhead,
    })
}

fn validate_dependencies(dependencies: &[Option<usize>]) -> Result<(), String> {
    for (pid, parent) in dependencies.iter().enumerate() {
        if let Some(parent) = parent {
            if *parent >= dependencies.len() {
                return Err(format!(
                    "process P{} depends on an unknown process",
                    pid + 1
                ));
            }
            if *parent == pid {
                return Err(format!("process P{} cannot depend on itself", pid + 1));
            }
        }
    }
    fn visit(pid: usize, dependencies: &[Option<usize>], state: &mut [u8]) -> bool {
        if state[pid] == 1 {
            return false;
        }
        if state[pid] == 2 {
            return true;
        }
        state[pid] = 1;
        if let Some(parent) = dependencies[pid] {
            if !visit(parent, dependencies, state) {
                return false;
            }
        }
        state[pid] = 2;
        true
    }
    let mut state = vec![0; dependencies.len()];
    for pid in 0..dependencies.len() {
        if !visit(pid, dependencies, &mut state) {
            return Err("process dependencies contain a cycle".into());
        }
    }
    Ok(())
}
