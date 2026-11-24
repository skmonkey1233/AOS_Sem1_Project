use crate::job::Job;

/// Small, dependency-free xorshift64* generator so runs are reproducible from a seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform integer in `lo..=hi`.
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as u32
    }
}

fn job(pid: usize, arrival: u32, burst: u32, priority: u32) -> Job {
    Job {
        pid,
        name: format!("P{}", pid + 1),
        arrival,
        burst,
        priority,
    }
}

fn from_rows(rows: &[(u32, u32, u32)]) -> Vec<Job> {
    rows.iter()
        .enumerate()
        .map(|(i, &(a, b, p))| job(i, a, b, p))
        .collect()
}

pub const DEMOS: [(&str, &str); 5] = [
    (
        "classic",
        "4 jobs from the textbook SRTF example, with priorities",
    ),
    (
        "convoy",
        "one long job ahead of two short ones (FCFS convoy effect)",
    ),
    (
        "starvation",
        "a steady stream of high-priority jobs vs one low-priority job",
    ),
    (
        "mixed",
        "a realistic mix of short interactive and long batch jobs",
    ),
    (
        "dependencies",
        "a build pipeline where jobs wait for a prerequisite to finish",
    ),
];

/// Prerequisite list for one workload: `deps[i]` is the index of the job that
/// must complete before job `i` may enter the ready queue.
pub type Dependencies = Vec<Option<usize>>;

/// Like [`demo`], but also returns the demo's process prerequisites
/// (all `None` for demos without dependencies).
pub fn demo_with_dependencies(name: &str) -> Option<(Vec<Job>, Dependencies)> {
    if name == "dependencies" {
        // Same example as the browser's "Process dependencies" preset.
        let rows: [(&str, u32, u32, u32, Option<usize>); 4] = [
            ("Build library", 0, 3, 2, None),
            ("Compile app", 0, 4, 1, Some(0)),
            ("Run tests", 0, 2, 1, Some(1)),
            ("Write docs", 1, 3, 3, Some(0)),
        ];
        let jobs = rows
            .iter()
            .enumerate()
            .map(|(i, &(n, a, b, p, _))| Job {
                pid: i,
                name: n.to_string(),
                arrival: a,
                burst: b,
                priority: p,
            })
            .collect();
        let deps = rows.iter().map(|r| r.4).collect();
        return Some((jobs, deps));
    }
    let jobs = demo(name)?;
    let deps = vec![None; jobs.len()];
    Some((jobs, deps))
}

pub fn demo(name: &str) -> Option<Vec<Job>> {
    match name {
        // (arrival, burst, priority)
        "classic" => Some(from_rows(&[(0, 8, 3), (1, 4, 1), (2, 9, 4), (3, 5, 2)])),
        "convoy" => Some(from_rows(&[(0, 24, 1), (0, 3, 1), (0, 3, 1)])),
        "starvation" => {
            let mut rows = vec![(0, 6, 4)];
            for t in 0..12 {
                rows.push((t * 2, 3, 1));
            }
            Some(from_rows(&rows))
        }
        "mixed" => Some(random(12, 7, "mixed")),
        "dependencies" => demo_with_dependencies(name).map(|(j, _)| j),
        _ => None,
    }
}

pub const SCENARIOS: [&str; 4] = ["mixed", "interactive", "cpu-bound", "bursty"];

/// Seeded random workload.
pub fn random(count: usize, seed: u64, scenario: &str) -> Vec<Job> {
    let mut rng = Rng::new(seed);
    let mut t = 0u32;
    let mut jobs = Vec::with_capacity(count);
    for i in 0..count {
        let (gap, burst) = match scenario {
            "interactive" => (rng.range(0, 2), rng.range(1, 4)),
            "cpu-bound" => (rng.range(0, 3), rng.range(20, 50)),
            "bursty" => {
                // Jobs arrive in clumps of ~4 at the same instant.
                let gap = if i % 4 == 0 { rng.range(8, 20) } else { 0 };
                (gap, rng.range(2, 18))
            }
            // "mixed": 70% short, 30% long
            _ => {
                let burst = if rng.range(1, 10) <= 7 {
                    rng.range(1, 6)
                } else {
                    rng.range(15, 40)
                };
                (rng.range(0, 4), burst)
            }
        };
        t += gap;
        jobs.push(job(i, t, burst, rng.range(1, 5)));
    }
    jobs
}

/// Parse `name,arrival,burst[,priority]` lines. A header line and `#` comments are allowed.
/// A fifth `depends_on` column is accepted but ignored here; use
/// [`parse_csv_with_dependencies`] to read it.
pub fn parse_csv(text: &str) -> Result<Vec<Job>, String> {
    parse_csv_with_dependencies(text).map(|(jobs, _)| jobs)
}

/// Parse `name,arrival,burst[,priority[,depends_on]]` lines. A header line and
/// `#` comments are allowed. `depends_on` is the *name* of another job in the
/// file (it may appear later in the file); blank, `none` or `-` means no
/// prerequisite. A blank priority defaults to 1.
pub fn parse_csv_with_dependencies(text: &str) -> Result<(Vec<Job>, Dependencies), String> {
    let mut jobs: Vec<Job> = Vec::new();
    // (line number, prerequisite name) for each job, resolved once all names are known.
    let mut wanted: Vec<Option<(usize, String)>> = Vec::new();
    for (ln, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(|c| c.trim()).collect();
        // Skip a header row such as "name,arrival,burst,priority,depends_on".
        if cols.len() >= 3 && cols[1].parse::<u32>().is_err() && jobs.is_empty() {
            continue;
        }
        if cols.len() < 3 || cols.len() > 5 {
            return Err(format!(
                "line {}: expected name,arrival,burst[,priority[,depends_on]] but got '{}'",
                ln + 1,
                line
            ));
        }
        let num = |s: &str, what: &str| -> Result<u32, String> {
            s.parse::<u32>().map_err(|_| {
                format!(
                    "line {}: {} '{}' is not a non-negative integer",
                    ln + 1,
                    what,
                    s
                )
            })
        };
        if cols[0].is_empty() {
            return Err(format!("line {}: job name is empty", ln + 1));
        }
        let arrival = num(cols[1], "arrival")?;
        let burst = num(cols[2], "burst")?;
        if burst == 0 {
            return Err(format!("line {}: burst must be at least 1", ln + 1));
        }
        let priority = match cols.get(3) {
            Some(p) if !p.is_empty() => num(p, "priority")?,
            _ => 1,
        };
        let dep = match cols.get(4) {
            Some(d) if !d.is_empty() && !d.eq_ignore_ascii_case("none") && *d != "-" => {
                Some((ln + 1, d.to_string()))
            }
            _ => None,
        };
        jobs.push(Job {
            pid: jobs.len(),
            name: cols[0].to_string(),
            arrival,
            burst,
            priority,
        });
        wanted.push(dep);
    }
    if jobs.is_empty() {
        return Err("no jobs found in file".into());
    }

    let mut deps: Dependencies = vec![None; jobs.len()];
    for (pid, w) in wanted.iter().enumerate() {
        let Some((line, name)) = w else { continue };
        let matches: Vec<usize> = jobs
            .iter()
            .filter(|j| &j.name == name)
            .map(|j| j.pid)
            .collect();
        match matches.as_slice() {
            [] => {
                return Err(format!(
                    "line {}: '{}' depends on '{}', but no job has that name",
                    line, jobs[pid].name, name
                ))
            }
            [parent] if *parent == pid => {
                return Err(format!("line {}: '{}' cannot depend on itself", line, name))
            }
            [parent] => deps[pid] = Some(*parent),
            _ => {
                return Err(format!(
                    "line {}: '{}' depends on '{}', but several jobs have that name; give them unique names",
                    line, jobs[pid].name, name
                ))
            }
        }
    }
    // Each job has at most one prerequisite, so following the chain from any job
    // must end within `n` steps unless it loops.
    for start in 0..jobs.len() {
        let mut cur = start;
        for _ in 0..=jobs.len() {
            match deps[cur] {
                Some(p) => cur = p,
                None => break,
            }
            if cur == start {
                let mut cycle = vec![jobs[start].name.clone()];
                let mut c = deps[start].unwrap();
                while c != start {
                    cycle.push(jobs[c].name.clone());
                    c = deps[c].unwrap();
                }
                cycle.push(jobs[start].name.clone());
                return Err(format!(
                    "job dependencies form a cycle: {}",
                    cycle.join(" -> ")
                ));
            }
        }
    }
    Ok((jobs, deps))
}

pub fn to_csv(jobs: &[Job]) -> String {
    let mut s = String::from("name,arrival,burst,priority\n");
    for j in jobs {
        s.push_str(&format!(
            "{},{},{},{}\n",
            j.name, j.arrival, j.burst, j.priority
        ));
    }
    s
}

/// Like [`to_csv`], but adds a `depends_on` column when any job has a prerequisite.
pub fn to_csv_with_dependencies(jobs: &[Job], deps: &[Option<usize>]) -> String {
    if deps.iter().all(Option::is_none) {
        return to_csv(jobs);
    }
    let mut s = String::from("name,arrival,burst,priority,depends_on\n");
    for (j, d) in jobs.iter().zip(deps) {
        let parent = d.map(|p| jobs[p].name.as_str()).unwrap_or("");
        s.push_str(&format!(
            "{},{},{},{},{}\n",
            j.name, j.arrival, j.burst, j.priority, parent
        ));
    }
    s
}
