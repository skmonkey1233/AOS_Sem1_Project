use sjs::engine::{simulate_with_dependencies, SimResult};
use sjs::gantt;
use sjs::job::Job;
use sjs::metrics::{analyze, Summary};
use sjs::policy::{Algo, Config};
use sjs::smart::{benchmark_with_dependencies, heuristic, profile, Goal};
use sjs::workload::{self, Dependencies};
use std::collections::HashMap;
use std::process::exit;
mod server;

const HELP: &str = "\
Smart Job Scheduling System - CPU scheduling simulator

USAGE:
    sjs <command> [options]

COMMANDS:
    run <algo>     Simulate one algorithm: Gantt chart, per-job table, summary
    compare        Run every algorithm on the same workload and compare them
    smart          Profile the workload, then benchmark and pick the best algorithm
    gen            Print a random workload as CSV (redirect into a file)
    list           List algorithms, demo workloads and scenarios
    serve          Launch the browser simulator backed by this Rust engine

WORKLOAD (pick one; default is --demo classic):
    --file <path>        CSV with lines: name,arrival,burst[,priority[,depends_on]]
                         depends_on = name of a job that must finish first
    --demo <name>        built-in workload (see `sjs list`)
    --random <n>         n random jobs   [--seed <u64>] [--scenario <name>]

OPTIONS:
    --quantum <n>        Round Robin time quantum            (default 4)
    --mlfq <a,b,c>       MLFQ quantum per level              (default 2,4,8)
    --boost <n>          MLFQ priority-boost period, 0=off   (default 50)
    --age <n>            Priority aging: +1 priority per n units waited (default 5)
    --cs <n>             Context-switch cost in time units   (default 0)
    --goal <g>           smart: balanced|throughput|interactive|fair (default balanced)
    --no-gantt           run: hide the Gantt chart
    --count <n>          gen: number of jobs (same as --random); gen also accepts --demo/--file
    --port <n>           serve: local web server port (default 8000)
    -h, --help           Show this help
";

struct Opts {
    values: HashMap<String, String>,
    flags: Vec<String>,
    positional: Vec<String>,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        values: HashMap::new(),
        flags: Vec::new(),
        positional: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-h" || a == "--help" {
            o.flags.push("help".into());
        } else if let Some(name) = a.strip_prefix("--") {
            if name == "no-gantt" {
                o.flags.push(name.into());
            } else {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| format!("option --{} needs a value", name))?;
                o.values.insert(name.to_string(), v.clone());
            }
        } else {
            o.positional.push(a.clone());
        }
        i += 1;
    }
    Ok(o)
}

impl Opts {
    fn num<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T, String> {
        match self.values.get(key) {
            None => Ok(default),
            Some(v) => v
                .parse::<T>()
                .map_err(|_| format!("--{} expects a number, got '{}'", key, v)),
        }
    }
}

fn config(o: &Opts) -> Result<Config, String> {
    let mut cfg = Config::default();
    cfg.quantum = o.num("quantum", cfg.quantum)?;
    cfg.boost = o.num("boost", cfg.boost)?;
    cfg.age_every = o.num("age", cfg.age_every)?;
    if let Some(s) = o.values.get("mlfq") {
        let q: Result<Vec<u32>, _> = s.split(',').map(|x| x.trim().parse::<u32>()).collect();
        cfg.mlfq_quanta =
            q.map_err(|_| format!("--mlfq expects numbers like 2,4,8, got '{}'", s))?;
        if cfg.mlfq_quanta.is_empty() {
            return Err("--mlfq needs at least one level".into());
        }
    }
    Ok(cfg)
}

/// The workload to simulate, its process prerequisites, and a label for output.
struct Loaded {
    jobs: Vec<Job>,
    deps: Dependencies,
    label: String,
}

fn load_workload(o: &Opts) -> Result<Loaded, String> {
    if let Some(path) = o.values.get("file") {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read '{}': {}", path, e))?;
        let (jobs, deps) =
            workload::parse_csv_with_dependencies(&text).map_err(|e| format!("{}: {}", path, e))?;
        return Ok(Loaded {
            jobs,
            deps,
            label: format!("file {}", path),
        });
    }
    if let Some(n) = o.values.get("random").or_else(|| o.values.get("count")) {
        let n: usize = n
            .parse()
            .map_err(|_| format!("--random expects a number, got '{}'", n))?;
        if n == 0 {
            return Err("--random needs at least 1 job".into());
        }
        let seed: u64 = o.num("seed", 1u64)?;
        let sc = o
            .values
            .get("scenario")
            .map(String::as_str)
            .unwrap_or("mixed");
        if !workload::SCENARIOS.contains(&sc) {
            return Err(format!(
                "unknown scenario '{}' (choose: {})",
                sc,
                workload::SCENARIOS.join(", ")
            ));
        }
        let jobs = workload::random(n, seed, sc);
        return Ok(Loaded {
            deps: vec![None; jobs.len()],
            jobs,
            label: format!("{} random '{}' jobs, seed {}", n, sc, seed),
        });
    }
    let name = o
        .values
        .get("demo")
        .map(String::as_str)
        .unwrap_or("classic");
    workload::demo_with_dependencies(name)
        .map(|(jobs, deps)| Loaded {
            jobs,
            deps,
            label: format!("demo '{}'", name),
        })
        .ok_or_else(|| format!("unknown demo '{}' (see `sjs list`)", name))
}

fn has_deps(deps: &[Option<usize>]) -> bool {
    deps.iter().any(Option::is_some)
}

/// Width of the job-name column: at least 8, wide enough for the longest name.
fn name_width(jobs: &[Job]) -> usize {
    jobs.iter()
        .map(|j| j.name.chars().count() + 2)
        .max()
        .unwrap_or(0)
        .max(8)
}

fn print_workload(w: &Loaded) {
    let nw = name_width(&w.jobs);
    println!("Workload: {}", w.label);
    let deps = has_deps(&w.deps);
    print!(
        "{:<nw$}{:>9}{:>7}{:>10}",
        "Job",
        "Arrival",
        "Burst",
        "Priority",
        nw = nw
    );
    println!("{}", if deps { "  Depends on" } else { "" });
    for (j, d) in w.jobs.iter().zip(&w.deps) {
        print!(
            "{:<nw$}{:>9}{:>7}{:>10}",
            j.name,
            j.arrival,
            j.burst,
            j.priority,
            nw = nw
        );
        if deps {
            print!("  {}", d.map(|p| w.jobs[p].name.as_str()).unwrap_or("-"));
        }
        println!();
    }
    if deps {
        println!("\nA job enters the ready queue only after it has arrived AND its prerequisite has finished.");
        println!("Waiting times below include time spent blocked on a prerequisite.");
    }
    println!();
}

fn print_run(res: &SimResult, show_gantt: bool) {
    let (rows, s) = analyze(res);
    let nw = rows
        .iter()
        .map(|m| m.name.chars().count() + 2)
        .max()
        .unwrap_or(0)
        .max(8);
    println!("== {} ==\n", res.policy);
    if show_gantt {
        println!("Gantt chart (idle = CPU idle, cs = context-switch overhead):\n");
        println!("{}", gantt::render(res));
    }
    println!(
        "{:<nw$}{:>8}{:>7}{:>7}{:>8}{:>6}{:>6}{:>6}",
        "Job",
        "Arrive",
        "Burst",
        "Prio",
        "Finish",
        "TAT",
        "Wait",
        "Resp",
        nw = nw
    );
    for m in &rows {
        println!(
            "{:<nw$}{:>8}{:>7}{:>7}{:>8}{:>6}{:>6}{:>6}",
            m.name,
            m.arrival,
            m.burst,
            m.priority,
            m.completion,
            m.turnaround,
            m.waiting,
            m.response,
            nw = nw
        );
    }
    println!();
    println!("Avg waiting time     : {:.2}", s.avg_wait);
    println!("Avg turnaround time  : {:.2}", s.avg_tat);
    println!("Avg response time    : {:.2}", s.avg_resp);
    println!("Max waiting time     : {}", s.max_wait);
    println!("CPU utilisation      : {:.1}%", s.cpu_util);
    println!("Throughput           : {:.3} jobs/unit", s.throughput);
    println!(
        "Context switches     : {} (overhead {} units)",
        s.context_switches, s.overhead
    );
    println!("Fairness (Jain)      : {:.3}", s.jain);
}

fn table_header() {
    println!(
        "{:<10}{:>9}{:>9}{:>9}{:>9}{:>8}{:>8}{:>7}{:>7}",
        "Algorithm", "AvgWait", "AvgTAT", "AvgResp", "MaxWait", "CPU%", "Thrpt", "CS", "Jain"
    );
}

fn table_row(name: &str, s: &Summary) {
    println!(
        "{:<10}{:>9.2}{:>9.2}{:>9.2}{:>9}{:>8.1}{:>8.3}{:>7}{:>7.3}",
        name,
        s.avg_wait,
        s.avg_tat,
        s.avg_resp,
        s.max_wait,
        s.cpu_util,
        s.throughput,
        s.context_switches,
        s.jain
    );
}

fn cmd_run(o: &Opts) -> Result<(), String> {
    let id = o
        .positional
        .get(1)
        .ok_or("usage: sjs run <algo>   (see `sjs list`)")?;
    let algo =
        Algo::parse(id).ok_or_else(|| format!("unknown algorithm '{}' (see `sjs list`)", id))?;
    let cfg = config(o)?;
    let cs: u32 = o.num("cs", 0)?;
    let w = load_workload(o)?;
    print_workload(&w);
    let mut pol = algo.build(&cfg);
    let res = simulate_with_dependencies(&w.jobs, &w.deps, pol.as_mut(), cs)?;
    print_run(&res, !o.flags.iter().any(|f| f == "no-gantt"));
    Ok(())
}

fn cmd_compare(o: &Opts) -> Result<(), String> {
    let cfg = config(o)?;
    let cs: u32 = o.num("cs", 0)?;
    let w = load_workload(o)?;
    print_workload(&w);
    table_header();
    for algo in Algo::ALL {
        let mut pol = algo.build(&cfg);
        let res = simulate_with_dependencies(&w.jobs, &w.deps, pol.as_mut(), cs)?;
        let (_, s) = analyze(&res);
        table_row(algo.short(), &s);
    }
    println!(
        "\nLower is better for AvgWait/AvgTAT/AvgResp/MaxWait/CS; higher for CPU%/Thrpt/Jain."
    );
    if cs > 0 {
        println!(
            "Context-switch cost of {} unit(s) is included in every row.",
            cs
        );
    }
    Ok(())
}

fn cmd_smart(o: &Opts) -> Result<(), String> {
    let cfg = config(o)?;
    let cs: u32 = o.num("cs", 0)?;
    let goal = match o.values.get("goal") {
        None => Goal::Balanced,
        Some(g) => Goal::parse(g).ok_or_else(|| {
            format!(
                "unknown goal '{}' (balanced|throughput|interactive|fair)",
                g
            )
        })?,
    };
    let w = load_workload(o)?;
    print_workload(&w);
    let jobs = &w.jobs;

    let p = profile(jobs);
    println!("Step 1: workload profile");
    println!("  jobs               : {}", p.jobs);
    println!("  mean burst         : {:.1}", p.mean_burst);
    println!(
        "  burst variability  : {:.2} (coefficient of variation)",
        p.burst_cv
    );
    println!("  short-job share    : {:.0}%", p.short_ratio * 100.0);
    println!("  priority levels    : {}", p.priority_levels);
    println!("  arrival span       : {} units", p.arrival_span);
    let blocked = w.deps.iter().filter(|d| d.is_some()).count();
    if blocked > 0 {
        println!("  with prerequisite  : {} of {} jobs", blocked, p.jobs);
    }
    println!();

    let (h_algo, why) = heuristic(&p, goal);
    println!("Step 2: rule-based suggestion for goal '{}'", goal.name());
    println!("  -> {} : {}\n", h_algo.short(), why);

    let ranked = benchmark_with_dependencies(jobs, &w.deps, &cfg, cs, goal)?;
    println!("Step 3: simulate every eligible algorithm and rank them (score 1.00 = ideal)");
    println!(
        "{:<5}{:<10}{:>8}{:>9}{:>9}{:>9}{:>7}{:>7}",
        "Rank", "Algorithm", "Score", "AvgWait", "AvgResp", "MaxWait", "CS", "Jain"
    );
    for (i, r) in ranked.iter().enumerate() {
        println!(
            "{:<5}{:<10}{:>8.3}{:>9.2}{:>9.2}{:>9}{:>7}{:>7.3}",
            i + 1,
            r.algo.short(),
            r.score,
            r.summary.avg_wait,
            r.summary.avg_resp,
            r.summary.max_wait,
            r.summary.context_switches,
            r.summary.jain
        );
    }

    let best = &ranked[0];
    println!("\nDecision: use {} for this workload.", best.algo.short());
    if best.algo == h_algo {
        println!("The benchmark confirms the rule-based suggestion.");
    } else {
        println!(
            "The benchmark overrides the rule-based suggestion ({}): measured results beat rules of thumb.",
            h_algo.short()
        );
    }
    println!();
    let mut pol = best.algo.build(&cfg);
    let res = simulate_with_dependencies(jobs, &w.deps, pol.as_mut(), cs)?;
    println!("Gantt chart of the chosen schedule:\n");
    println!("{}", gantt::render(&res));
    Ok(())
}

fn cmd_gen(o: &Opts) -> Result<(), String> {
    let mut o2 = Opts {
        values: o.values.clone(),
        flags: vec![],
        positional: vec![],
    };
    let picked = ["random", "count", "demo", "file"]
        .iter()
        .any(|k| o2.values.contains_key(*k));
    if !picked {
        o2.values.insert("random".into(), "10".into());
    }
    let w = load_workload(&o2)?;
    print!("{}", workload::to_csv_with_dependencies(&w.jobs, &w.deps));
    Ok(())
}

fn cmd_serve(o: &Opts) -> Result<(), String> {
    let port = o.num("port", 8000u16)?;
    server::serve(port)
}

fn cmd_list() {
    println!("Algorithms:");
    for a in Algo::ALL {
        println!("  {:<15} {}", a.id(), a.describe());
    }
    println!("\nDemo workloads (--demo):");
    for (n, d) in workload::DEMOS {
        println!("  {:<12} {}", n, d);
    }
    println!("\nRandom scenarios (--random N --scenario):");
    println!("  {}", workload::SCENARIOS.join(", "));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_opts(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {}", e);
            exit(2);
        }
    };
    let cmd = opts.positional.first().map(String::as_str).unwrap_or("");
    if opts.flags.iter().any(|f| f == "help") || cmd.is_empty() || cmd == "help" {
        print!("{}", HELP);
        return;
    }
    let result = match cmd {
        "run" => cmd_run(&opts),
        "compare" => cmd_compare(&opts),
        "smart" => cmd_smart(&opts),
        "gen" => cmd_gen(&opts),
        "serve" => cmd_serve(&opts),
        "list" => {
            cmd_list();
            Ok(())
        }
        other => Err(format!("unknown command '{}'. Try `sjs --help`.", other)),
    };
    if let Err(e) = result {
        eprintln!("error: {}", e);
        exit(1);
    }
}
