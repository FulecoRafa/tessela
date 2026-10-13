//! Module `system`: CPU, memory, disk, temperature and battery from `/proc`
//! and `/sys`, read every two seconds while someone is subscribed
//! (capability `system`).

use std::path::Path;
use std::sync::{Arc, Once};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tessela_core::{Lang, Module, ModuleCx};

use crate::topic::{JS_TOPIC, LUAU_TOPIC, Topics};

const PERIOD: Duration = Duration::from_secs(2);
const HISTORY: usize = 30;
const TOPICS: &[&str] = &[
    "cpu",
    "cpuHistory",
    "memory",
    "disk",
    "temperature",
    "battery",
];

/// `(busy, total)` jiffies from the first line of /proc/stat.
fn cpu_times(stat: &str) -> Option<(u64, u64)> {
    let fields: Vec<u64> = stat
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .filter_map(|f| f.parse().ok())
        .collect();
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    let total: u64 = fields.iter().sum();
    Some((total - idle, total))
}

fn gib(kib: f64) -> f64 {
    (kib / 1_048_576.0 * 10.0).round() / 10.0
}

fn memory(meminfo: &str) -> Value {
    let field = |name: &str| {
        meminfo
            .lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    let (total, available) = (field("MemTotal:"), field("MemAvailable:"));
    if total <= 0.0 {
        return Value::Null;
    }
    json!({
        "percent": ((1.0 - available / total) * 100.0).round(),
        "usedGiB": gib(total - available),
        "totalGiB": gib(total),
    })
}

fn disk(path: &str) -> Value {
    let Ok(c) = std::ffi::CString::new(path) else {
        return Value::Null;
    };
    // SAFETY: statvfs writes into the zeroed struct we own; `c` is a valid C string.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 || s.f_blocks == 0 {
        return Value::Null;
    }
    let block = s.f_frsize as f64 / 1024.0;
    let total = s.f_blocks as f64 * block;
    let used = (s.f_blocks - s.f_bfree) as f64 * block;
    json!({
        "percent": (used / total * 100.0).round(),
        "usedGiB": gib(used),
        "totalGiB": gib(total),
    })
}

fn temperature() -> Value {
    let Ok(zones) = std::fs::read_dir("/sys/class/thermal") else {
        return Value::Null;
    };
    let max = zones
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("thermal_zone"))
        .filter_map(|e| std::fs::read_to_string(e.path().join("temp")).ok())
        .filter_map(|t| t.trim().parse::<f64>().ok())
        .fold(f64::NAN, f64::max);
    if max.is_nan() {
        Value::Null
    } else {
        json!((max / 1000.0).round())
    }
}

fn read_num(dir: &Path, name: &str) -> Option<f64> {
    std::fs::read_to_string(dir.join(name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The first battery under /sys/class/power_supply: percent, charging,
/// seconds left (to empty, or to full while charging) and watts.
fn battery(root: &Path) -> Value {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Value::Null;
    };
    let Some(dir) = entries.flatten().map(|e| e.path()).find(|p| {
        std::fs::read_to_string(p.join("type")).is_ok_and(|t| t.trim() == "Battery")
            && read_num(p, "present").unwrap_or(1.0) > 0.0
    }) else {
        return Value::Null;
    };
    let status = std::fs::read_to_string(dir.join("status")).unwrap_or_default();
    let status = status.trim();
    let charging = matches!(status, "Charging" | "Full");
    // energy_* (µWh) with power_now (µW), or charge_* (µAh) with current_now (µA).
    let (now, full, rate) = match (read_num(&dir, "energy_now"), read_num(&dir, "energy_full")) {
        (Some(n), Some(f)) => (n, f, read_num(&dir, "power_now").unwrap_or(0.0)),
        _ => (
            read_num(&dir, "charge_now").unwrap_or(0.0),
            read_num(&dir, "charge_full").unwrap_or(0.0),
            read_num(&dir, "current_now").unwrap_or(0.0),
        ),
    };
    let percent =
        read_num(&dir, "capacity").unwrap_or(if full > 0.0 { now / full * 100.0 } else { 0.0 });
    let time_left = if rate > 0.0 {
        if charging {
            (full - now).max(0.0) / rate * 3600.0
        } else {
            now / rate * 3600.0
        }
    } else {
        0.0
    };
    let watts = match read_num(&dir, "power_now") {
        Some(p) => p / 1e6,
        None => rate / 1e6 * read_num(&dir, "voltage_now").unwrap_or(0.0) / 1e6,
    };
    json!({
        "percent": percent.round(),
        "charging": charging,
        "status": status,
        "timeLeft": time_left.round(),
        "watts": (watts * 10.0).round() / 10.0,
    })
}

pub struct System {
    topics: Arc<Topics>,
    started: Once,
    js: String,
    luau: String,
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

impl System {
    pub fn new() -> Self {
        let methods = TOPICS
            .iter()
            .map(|t| format!("    {t}: () => topic(\"system\", \"{t}\")(),\n"))
            .collect::<String>();
        let js = format!(
            "\"use strict\";\n(() => {{\n  const rt = globalThis.__rt;\n{JS_TOPIC}\n  rt.define(\"system\", Object.freeze({{\n{methods}  }}));\n}})();\n"
        );
        let methods = TOPICS
            .iter()
            .map(|t| format!("\t{t} = function() return topic(\"system\", \"{t}\")() end,\n"))
            .collect::<String>();
        let luau =
            format!("local rt = __rt\n{LUAU_TOPIC}\nsystem = table.freeze({{\n{methods}}})\n");
        Self {
            topics: Topics::new(),
            started: Once::new(),
            js,
            luau,
        }
    }

    fn start(&self) {
        let topics = self.topics.clone();
        thread::Builder::new()
            .name("system".into())
            .spawn(move || {
                let mut last: Option<(u64, u64)> = None;
                let mut history: Vec<f64> = Vec::new();
                loop {
                    if let Some((busy, total)) = std::fs::read_to_string("/proc/stat")
                        .ok()
                        .and_then(|s| cpu_times(&s))
                    {
                        if let Some((b0, t0)) = last
                            && total > t0
                        {
                            let cpu = ((busy - b0) as f64 / (total - t0) as f64 * 100.0).round();
                            history.push(cpu);
                            if history.len() > HISTORY {
                                history.remove(0);
                            }
                            topics.publish("cpu", json!(cpu));
                            topics.publish("cpuHistory", json!(history));
                        }
                        last = Some((busy, total));
                    }
                    if let Ok(m) = std::fs::read_to_string("/proc/meminfo") {
                        topics.publish("memory", memory(&m));
                    }
                    topics.publish("disk", disk("/"));
                    topics.publish("temperature", temperature());
                    topics.publish("battery", battery(Path::new("/sys/class/power_supply")));
                    // The first CPU sample needs two readings; take the second soon.
                    thread::sleep(if history.is_empty() {
                        Duration::from_millis(250)
                    } else {
                        PERIOD
                    });
                }
            })
            .expect("spawn system poller");
    }

    pub fn snapshot(&self) -> Value {
        TOPICS
            .iter()
            .map(|t| (t.to_string(), self.topics.get(t)))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }
}

impl Module for System {
    fn name(&self) -> &str {
        "system"
    }

    fn prelude(&self, lang: Lang) -> Option<&str> {
        Some(match lang {
            Lang::Luau => &self.luau,
            Lang::JavaScript => &self.js,
        })
    }

    fn invoke(&self, cx: &mut ModuleCx<'_>, func: &str, args: Value) -> Result<Value, String> {
        cx.require("system")?;
        match func {
            "subscribe" => {
                let topic = args["topic"].as_str().ok_or("missing topic")?;
                if !TOPICS.contains(&topic) {
                    return Err(format!("system: unknown topic '{topic}'"));
                }
                self.started.call_once(|| self.start());
                self.topics.subscribe(cx, topic)
            }
            other => Err(format!("system: unknown function '{other}'")),
        }
    }

    fn release(&self, key: u64) {
        self.topics.release(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_and_sys() {
        assert_eq!(
            cpu_times("cpu  10 0 10 70 10 0 0 0\ncpu0 1"),
            Some((20, 100))
        );
        let m = memory("MemTotal: 16000000 kB\nMemFree: 1 kB\nMemAvailable: 8000000 kB\n");
        assert_eq!(m["percent"], 50.0);

        let dir = std::env::temp_dir().join(format!("tessela-bat-{}", std::process::id()));
        let bat = dir.join("BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        for (k, v) in [
            ("type", "Battery"),
            ("status", "Discharging"),
            ("capacity", "50"),
            ("energy_now", "25000000"),
            ("energy_full", "50000000"),
            ("power_now", "10000000"),
        ] {
            std::fs::write(bat.join(k), v).unwrap();
        }
        let b = battery(&dir);
        assert_eq!(b["percent"], 50.0);
        assert_eq!(b["charging"], false);
        assert_eq!(b["timeLeft"], 9000.0); // 25 Wh at 10 W = 2.5 h
        assert_eq!(b["watts"], 10.0);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(disk("/")["totalGiB"].as_f64().unwrap() > 0.0);
    }
}
