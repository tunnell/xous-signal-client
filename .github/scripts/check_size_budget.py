#!/usr/bin/env python3
"""Check xous-signal-client Xous binary against .size-budget.toml. Exit 1 on hard breach."""
import argparse
import csv
import fnmatch
import json
import subprocess
import sys
from pathlib import Path

try:
    import tomllib  # Python 3.11+
except ImportError:
    try:
        import tomli as tomllib  # pip install tomli
    except ImportError:
        sys.exit("error: install tomllib (Python 3.11+) or tomli (pip install tomli)")


# ---------- size measurement helpers ----------

def readelf_load_vm(elf: str) -> int:
    """Sum of MemSiz for all PT_LOAD segments (total resident VM footprint)."""
    out = subprocess.check_output(
        ["riscv64-unknown-elf-readelf", "-l", "--wide", elf],
        text=True
    )
    total = 0
    for line in out.splitlines():
        parts = line.split()
        if parts and parts[0] == "LOAD":
            # readelf -l output: Type Offset VirtAddr PhysAddr FileSiz MemSiz Flg Align
            try:
                total += int(parts[5], 16)
            except (IndexError, ValueError):
                pass
    return total


def section_sizes(elf: str) -> dict[str, int]:
    """Map from section name → size in bytes via riscv64-unknown-elf-size -A."""
    out = subprocess.check_output(
        ["riscv64-unknown-elf-size", "-A", elf],
        text=True
    )
    sizes = {}
    for line in out.splitlines()[1:]:  # skip header
        parts = line.split()
        if len(parts) >= 2:
            try:
                sizes[parts[0]] = int(parts[1])
            except ValueError:
                pass
    return sizes


def crate_sizes(target: str, bin_name: str, features: str | None) -> list[dict]:
    """Run cargo bloat --crates and return JSON list of {name, size} entries."""
    cmd = [
        "cargo", "bloat", "--release",
        "--target", target,
        "--bin", bin_name,
        "--crates", "-n", "0",
        "--message-format", "json",
    ]
    if features:
        cmd += ["--features", features]
    env_extras = {
        "CC_riscv32imac_unknown_xous_elf": "riscv64-unknown-elf-gcc",
        "AR_riscv32imac_unknown_xous_elf": "riscv64-unknown-elf-ar",
        "CFLAGS_riscv32imac_unknown_xous_elf": "-march=rv32imac -mabi=ilp32",
    }
    import os
    env = {**os.environ, **env_extras}
    out = subprocess.check_output(cmd, text=True, env=env)
    return json.loads(out).get("crates", [])


# ---------- formatting ----------

def fmt_bytes(n: int | float) -> str:
    n = float(n)
    for unit in ("B", "KiB", "MiB"):
        if abs(n) < 1024:
            return f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} GiB"


def pct(value: int, hard: int) -> str:
    return f"{100 * value / hard:.1f}%"


def find_budget(name: str, table: dict) -> dict | None:
    if name in table:
        return table[name]
    for pattern, value in table.items():
        if "*" in pattern and fnmatch.fnmatch(name, pattern):
            return value
    return None


# ---------- main ----------

def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--budget", required=True, help="Path to .size-budget.toml")
    ap.add_argument("--binary", required=True, help="Path to the ELF binary")
    ap.add_argument("--target", required=True, help="Cargo target triple")
    ap.add_argument("--bin-name", required=True, help="Binary name for cargo bloat")
    ap.add_argument("--report-md", required=True, help="Output markdown report path")
    ap.add_argument("--features", default=None, help="Cargo features for cargo bloat")
    args = ap.parse_args()

    cfg = tomllib.loads(Path(args.budget).read_text())
    lines: list[str] = ["<!-- size-budget-marker -->", "### Size budget — xous-signal-client on Xous\n"]
    breaches: list[str] = []
    warnings: list[str] = []

    # --- Total LOAD VM ---
    total = readelf_load_vm(args.binary)
    hard = cfg["budget"]["total"]["hard"]
    soft = cfg["budget"]["total"].get("soft_warn")
    lines.append(
        f"**Total LOAD VM:** `{fmt_bytes(total)}` / hard `{fmt_bytes(hard)}` "
        f"({pct(total, hard)})\n"
    )
    if total > hard:
        breaches.append(f"TOTAL {fmt_bytes(total)} > hard limit {fmt_bytes(hard)}")
    elif soft and total > soft:
        warnings.append(f"Total LOAD VM ({fmt_bytes(total)}) > soft warn {fmt_bytes(soft)}")

    # --- Sections ---
    secs = section_sizes(args.binary)
    lines.append("#### Sections\n| section | measured | hard | % of hard | status |")
    lines.append("|---|---:|---:|---:|---|")
    for name, b in cfg["budget"]["sections"].items():
        sz = secs.get(name, 0)
        h = b["hard"]
        ok = sz <= h
        if not ok:
            breaches.append(f"section {name}: {fmt_bytes(sz)} > hard {fmt_bytes(h)}")
        lines.append(
            f"| `{name}` | {fmt_bytes(sz)} | {fmt_bytes(h)} | {pct(sz, h)} | "
            f"{'✅' if ok else '❌ OVER'} |"
        )

    # --- Crates ---
    crates_data = crate_sizes(args.target, args.bin_name, args.features)
    crates_map = {e["name"]: e["size"] for e in crates_data}
    lines.append("\n#### Crates (.text)\n| crate | measured | hard | % of hard | status |")
    lines.append("|---|---:|---:|---:|---|")
    for crate_name, b in cfg["budget"]["crates"].items():
        sz = crates_map.get(crate_name, 0)
        h = b["hard"]
        ok = sz <= h
        if not ok:
            breaches.append(f"crate {crate_name}: {fmt_bytes(sz)} > hard {fmt_bytes(h)}")
        lines.append(
            f"| `{crate_name}` | {fmt_bytes(sz)} | {fmt_bytes(h)} | {pct(sz, h)} | "
            f"{'✅' if ok else '❌ OVER'} |"
        )

    # --- Summary ---
    if warnings:
        lines.append("\n**⚠️ Warnings:**")
        lines.extend(f"- {w}" for w in warnings)
    if breaches:
        lines.append("\n**❌ Budget breaches:**")
        lines.extend(f"- {b}" for b in breaches)
    else:
        lines.append("\n✅ All hard budgets pass.")

    Path(args.report_md).write_text("\n".join(lines) + "\n")
    print(f"Report written to {args.report_md}")

    if breaches:
        for b in breaches:
            print(f"::error::{b}")
        sys.exit(1)


if __name__ == "__main__":
    main()
