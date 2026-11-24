use crate::engine::{Kind, SimResult};

const WIDTH: usize = 96;

/// Render the timeline as a wrapped ASCII Gantt chart:
///
/// ```text
/// | P1   | P2  | idle | P3 |
/// 0      6     9      12   15
/// ```
pub fn render(r: &SimResult) -> String {
    let cells: Vec<(String, u32, u32)> = r
        .timeline
        .iter()
        .map(|s| {
            let label = match s.kind {
                Kind::Run(pid) => r.procs[pid].job.name.clone(),
                Kind::Idle => "idle".to_string(),
                Kind::Switch => "cs".to_string(),
            };
            (label, s.start, s.end)
        })
        .collect();

    let mut out = String::new();
    let mut bar = String::new();
    let mut times = String::new();
    let mut last_end = 0u32;

    let flush = |bar: &mut String, times: &mut String, end: u32, out: &mut String| {
        bar.push('|');
        times.push_str(&end.to_string());
        out.push_str(bar);
        out.push('\n');
        out.push_str(times);
        out.push_str("\n\n");
        bar.clear();
        times.clear();
    };

    for (label, start, end) in cells {
        // Width grows with duration a little so long runs look longer.
        let dur = (end - start) as usize;
        let w = (label.len() + 2)
            .max(start.to_string().len() + 1)
            .max(dur.min(8) + 2);
        if bar.len() + w + 1 > WIDTH && !bar.is_empty() {
            flush(&mut bar, &mut times, start, &mut out);
        }
        bar.push('|');
        let pad = w - 1 - label.len();
        let left = pad / 2;
        bar.push_str(&" ".repeat(left));
        bar.push_str(&label);
        bar.push_str(&" ".repeat(pad - left));
        times.push_str(&format!("{:<width$}", start, width = w));
        last_end = end;
    }
    if !bar.is_empty() {
        flush(&mut bar, &mut times, last_end, &mut out);
    }
    out.trim_end().to_string() + "\n"
}
