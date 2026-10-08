#!/usr/bin/env python3
"""marathon (GPUI) vs Quickshell, same bar, same services, same compositor.

Runs each bar inside a headless sway (wlroots, layer-shell) and measures:
  - startup: process start -> first non-empty frame of the bar region
  - idle memory: RSS and PSS (whole process tree) after settling
  - idle CPU: % of one core over a window, with services ticking at 1 Hz
  - stress CPU: same, with an extra 100 Hz counter widget

Everything renders on the CPU (no GPU here: wlroots pixman + Mesa llvmpipe /
lavapipe), so absolute CPU numbers are inflated for both; compare ratios.

usage: bench.py [--runs N] [--idle SECS] [--stress SECS]
env:   QS_BIN (quickshell binary), MARATHON_BIN, SCRATCH (work dir)
"""

import argparse
import hashlib
import json
import os
import shutil
import signal
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRATCH = Path(os.environ.get("SCRATCH", "/tmp/marathon-bench"))
QS_BIN = os.environ.get("QS_BIN", "quickshell")
MARATHON_BIN = os.environ.get("MARATHON_BIN", str(ROOT / "target/release/marathon"))
TICK = os.sysconf("SC_CLK_TCK")
PAGE = os.sysconf("SC_PAGE_SIZE")
BAR = "0,0 1280x32"


def env_base():
    e = dict(os.environ)
    e.update(
        XDG_RUNTIME_DIR=str(SCRATCH / "xdg"),
        WAYLAND_DISPLAY="wayland-1",
        MARATHON_SERVICES_DIR=str(ROOT / "run/services"),
        VK_ICD_FILENAMES="/usr/share/vulkan/icd.d/lvp_icd.json",
        RUST_LOG="error",
    )
    e.pop("DISPLAY", None)
    # A desktop session doesn't set these; with them, any Rust error captures a backtrace.
    e.pop("RUST_BACKTRACE", None)
    e.pop("RUST_LIB_BACKTRACE", None)
    return e


def tree_pids(pid):
    pids, frontier = [pid], [pid]
    while frontier:
        p = frontier.pop()
        try:
            kids = Path(f"/proc/{p}/task/{p}/children").read_text().split()
        except OSError:
            continue
        for k in map(int, kids):
            pids.append(k)
            frontier.append(k)
    return pids


def cpu_ticks(pid):
    total = 0
    for p in tree_pids(pid):
        try:
            f = Path(f"/proc/{p}/stat").read_text().rsplit(")", 1)[1].split()
            total += int(f[11]) + int(f[12])  # utime + stime
        except (OSError, IndexError):
            pass
    return total


def memory(pid):
    rss = pss = 0
    for p in tree_pids(pid):
        try:
            for line in Path(f"/proc/{p}/smaps_rollup").read_text().splitlines():
                k, v = line.split(":", 1) if ":" in line else (line, "")
                if k == "Rss":
                    rss += int(v.split()[0])
                elif k == "Pss":
                    pss += int(v.split()[0])
        except OSError:
            pass
    return rss / 1024, pss / 1024  # MiB


def grab(env):
    out = subprocess.run(["grim", "-g", BAR, "-t", "ppm", "-"], env=env, capture_output=True)
    return out.stdout


def start_sway():
    xdg = SCRATCH / "xdg"
    shutil.rmtree(xdg, ignore_errors=True)
    xdg.mkdir(parents=True, mode=0o700)
    conf = SCRATCH / "sway.conf"
    conf.write_text("output HEADLESS-1 resolution 1280x200 bg #1b1f27 solid_color\ndefault_border none\nxwayland disable\n")
    env = env_base()
    env.update(WLR_BACKENDS="headless", WLR_RENDERER="pixman", WLR_LIBINPUT_NO_DEVICES="1")
    p = subprocess.Popen(["sway", "-c", str(conf)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        if (xdg / "wayland-1").exists():
            break
        time.sleep(0.05)
    time.sleep(0.3)
    return p


def start_services(stress):
    env = env_base()
    (ROOT / "run/services").mkdir(parents=True, exist_ok=True)
    procs = [
        subprocess.Popen([str(ROOT / "target/power")], env=env, stderr=subprocess.DEVNULL),
        subprocess.Popen([sys.executable, str(ROOT / "services/sys/sys_service.py")], env=env, stderr=subprocess.DEVNULL),
    ]
    if stress:
        procs.append(subprocess.Popen([sys.executable, str(ROOT / "services/stress/stress_service.py")], env=env, stderr=subprocess.DEVNULL))
    time.sleep(0.5)
    return procs


def command(bar, stress):
    if bar == "marathon":
        # Overridable to A/B two marathon builds with their own widget sets.
        cfg = os.environ.get("MARATHON_STRESS_CONFIG" if stress else "MARATHON_CONFIG")
        cfg = Path(cfg) if cfg else ROOT / ("bench/marathon-stress.toml" if stress else "bench/marathon.toml")
        return [MARATHON_BIN, "--config", str(cfg), "--gpui"], {}
    extra = {"BENCH_STRESS": "1"} if stress else {}
    return [QS_BIN, "-p", str(ROOT / "bench/quickshell")], extra


def run_once(bar, stress, settle, window):
    sway = start_sway()
    services = start_services(stress)
    env = env_base()
    empty = hashlib.sha1(grab(env)).hexdigest()

    cmd, extra = command(bar, stress)
    env.update(extra)
    log = open(SCRATCH / f"{bar}{'-stress' if stress else ''}.log", "w")
    t0 = time.perf_counter()
    proc = subprocess.Popen(cmd, env=env, stdout=log, stderr=log)
    first_frame = None
    while time.perf_counter() - t0 < 20:
        if proc.poll() is not None:
            break
        if hashlib.sha1(grab(env)).hexdigest() != empty:
            first_frame = time.perf_counter() - t0
            break
        time.sleep(0.01)

    result = {"bar": bar, "stress": stress, "startup_s": first_frame, "alive": proc.poll() is None}
    if result["alive"]:
        time.sleep(settle)
        c0, w0 = cpu_ticks(proc.pid), time.perf_counter()
        time.sleep(window)
        c1, w1 = cpu_ticks(proc.pid), time.perf_counter()
        rss, pss = memory(proc.pid)
        result.update(cpu_pct=100.0 * (c1 - c0) / TICK / (w1 - w0), rss_mib=rss, pss_mib=pss)
        shot = SCRATCH / f"{bar}{'-stress' if stress else ''}.png"
        subprocess.run(["grim", "-g", BAR, str(shot)], env=env)
    for p in [proc, *services, sway]:
        p.send_signal(signal.SIGTERM)
    for p in [proc, *services, sway]:
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()
    log.close()
    time.sleep(0.5)
    return result


def summarize(rows, key):
    vals = [r[key] for r in rows if r.get(key) is not None]
    if not vals:
        return "—"
    med = statistics.median(vals)
    lo, hi = min(vals), max(vals)
    return f"{med:.3g} ({lo:.3g}–{hi:.3g})"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--settle", type=float, default=8)
    ap.add_argument("--idle", type=float, default=30)
    ap.add_argument("--stress", type=float, default=20)
    ap.add_argument("--bars", default="marathon,quickshell")
    args = ap.parse_args()
    SCRATCH.mkdir(parents=True, exist_ok=True)

    results = []
    bars = args.bars.split(",")
    for i in range(args.runs):
        for bar in bars if i % 2 == 0 else reversed(bars):  # alternate order
            r = run_once(bar, False, args.settle, args.idle)
            print(json.dumps(r), flush=True)
            results.append(r)
    for bar in bars:
        r = run_once(bar, True, args.settle, args.stress)
        print(json.dumps(r), flush=True)
        results.append(r)

    (SCRATCH / "results.json").write_text(json.dumps(results, indent=2))
    print()
    print(f"{'':12} {'startup s':>22} {'RSS MiB':>22} {'PSS MiB':>22} {'CPU % idle':>22} {'CPU % 100Hz':>16}")
    for bar in bars:
        idle = [r for r in results if r["bar"] == bar and not r["stress"]]
        st = [r for r in results if r["bar"] == bar and r["stress"]]
        print(f"{bar:12} {summarize(idle, 'startup_s'):>22} {summarize(idle, 'rss_mib'):>22} "
              f"{summarize(idle, 'pss_mib'):>22} {summarize(idle, 'cpu_pct'):>22} {summarize(st, 'cpu_pct'):>16}")


if __name__ == "__main__":
    main()
