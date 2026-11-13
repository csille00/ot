//! This machine at a glance, as htop has it: each core's use, memory, swap,
//! load, and what is busiest, read from /proc every couple of seconds.
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// All the cores' use, 0 to 1, then each core's.
    pub cpu: f64,
    pub cores: Vec<f64>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load: [f64; 3],
    pub uptime: u64,
    pub procs: usize,
    /// The busiest processes: name, share of one core (1.0 is a whole one), memory.
    pub top: Vec<(String, f64, u64)>,
}

/// How often it is read.
pub const EVERY: Duration = Duration::from_secs(2);

/// Clock ticks a second, as the kernel counts CPU time in /proc.
const HZ: f64 = 100.0;

#[derive(Default)]
pub struct Sampler {
    cores: Vec<(u64, u64)>,
    procs: HashMap<u32, u64>,
    at: Option<Instant>,
}

/// A /proc/stat cpu line's busy and total ticks.
fn ticks(line: &str) -> Option<(u64, u64)> {
    let n: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if n.len() < 4 {
        return None;
    }
    let total: u64 = n.iter().take(8).sum();
    let idle = n[3] + n.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

fn share(now: (u64, u64), was: (u64, u64)) -> f64 {
    let (busy, total) = (now.0.saturating_sub(was.0), now.1.saturating_sub(was.1));
    if total == 0 {
        0.0
    } else {
        busy as f64 / total as f64
    }
}

impl Sampler {
    /// What has happened since the last read; the first read has no use yet.
    pub fn read(&mut self) -> Stats {
        let mut s = Stats::default();
        let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        let lines: Vec<(u64, u64)> = stat.lines().filter(|l| l.starts_with("cpu")).filter_map(ticks).collect();
        if let Some((&all, cores)) = lines.split_first() {
            let was = self.cores.clone();
            s.cpu = was.first().map_or(0.0, |&w| share(all, w));
            s.cores = cores.iter().enumerate().map(|(i, &c)| was.get(i + 1).map_or(0.0, |&w| share(c, w))).collect();
            self.cores = lines.clone();
        }
        let mem = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let kb = |key: &str| mem.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()).unwrap_or(0) * 1024;
        s.mem_total = kb("MemTotal:");
        s.mem_used = s.mem_total.saturating_sub(kb("MemAvailable:"));
        s.swap_total = kb("SwapTotal:");
        s.swap_used = s.swap_total.saturating_sub(kb("SwapFree:"));
        let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        for (i, v) in load.split_whitespace().take(3).enumerate() {
            s.load[i] = v.parse().unwrap_or(0.0);
        }
        s.uptime = std::fs::read_to_string("/proc/uptime").ok().and_then(|u| u.split('.').next()?.parse().ok()).unwrap_or(0);
        let since = self.at.map(|a| a.elapsed().as_secs_f64()).filter(|s| *s > 0.0);
        self.at = Some(Instant::now());
        let mut now: HashMap<u32, u64> = HashMap::new();
        let mut top = vec![];
        for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            let Ok(st) = std::fs::read_to_string(e.path().join("stat")) else { continue };
            // The name is in parentheses and may hold spaces; the fields follow it.
            let Some(close) = st.rfind(')') else { continue };
            let name = st[st.find('(').map_or(0, |i| i + 1)..close].to_string();
            let f: Vec<&str> = st[close + 1..].split_whitespace().collect();
            let (Some(u), Some(k), Some(rss)) = (f.get(11), f.get(12), f.get(21)) else { continue };
            let t = u.parse::<u64>().unwrap_or(0) + k.parse::<u64>().unwrap_or(0);
            now.insert(pid, t);
            let used = match (since, self.procs.get(&pid)) {
                (Some(secs), Some(&was)) => t.saturating_sub(was) as f64 / HZ / secs,
                _ => 0.0,
            };
            top.push((name, used, rss.parse::<u64>().unwrap_or(0) * 4096));
        }
        s.procs = now.len();
        self.procs = now;
        // The same program's processes together, as you'd name what is busy.
        let mut by_name: HashMap<String, (f64, u64)> = HashMap::new();
        for (name, used, mem) in top {
            let e = by_name.entry(name).or_default();
            e.0 += used;
            e.1 += mem;
        }
        let mut top: Vec<(String, f64, u64)> = by_name.into_iter().map(|(n, (c, m))| (n, c, m)).collect();
        top.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)));
        top.truncate(8);
        s.top = top;
        s
    }
}

/// Reads this machine every EVERY and hands each reading to `send`, until it says stop.
pub fn watch(send: impl Fn(Stats) -> bool + Send + 'static) {
    std::thread::spawn(move || {
        let mut s = Sampler::default();
        s.read();
        loop {
            std::thread::sleep(EVERY);
            if !send(s.read()) {
                return;
            }
        }
    });
}

/// Bytes as htop writes them: 12.1G, 800M.
pub fn size(b: u64) -> String {
    let g = b as f64 / (1u64 << 30) as f64;
    if g >= 1.0 {
        format!("{g:.1}G")
    } else {
        format!("{}M", b >> 20)
    }
}

/// Seconds up as "3d 4h", "5h 12m", "9m".
pub fn up(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cpu_line_reads_as_busy_and_total_ticks() {
        assert_eq!(ticks("cpu0 10 0 5 80 5 0 0 0 0 0"), Some((15, 100)));
        assert!((share((25, 200), (15, 100)) - 0.1).abs() < 1e-9);
    }

    #[test]
    fn a_second_read_sees_this_machine() {
        let mut s = Sampler::default();
        s.read();
        std::thread::sleep(Duration::from_millis(200));
        let r = s.read();
        assert!(r.mem_total > 0 && !r.cores.is_empty() && r.procs > 0);
    }
}
