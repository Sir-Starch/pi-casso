<div align="center">

# π-casso

### Finding pictures in π because useful software was apparently already taken

A fast, resumable Rust CLI and TUI that scans real digits of π for patterns resembling ASCII art.

[![Status: gloriously useless](https://img.shields.io/badge/status-gloriously%20useless-ff69b4)](#why)
[![CI](https://github.com/Sir-Starch/pi-casso/actions/workflows/ci.yml/badge.svg)](https://github.com/Sir-Starch/pi-casso/actions/workflows/ci.yml)
[![Rust 2024](https://img.shields.io/badge/Rust-2024-000000?logo=rust)](https://www.rust-lang.org/)
[![Terminal UI](https://img.shields.io/badge/interface-TUI-4EAA25?logo=gnometerminal&logoColor=white)](#terminal-ui)
[![GPU acceleration](https://img.shields.io/badge/GPU-optional-7B68EE)](#performance-and-gpu)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[Quick start](#quick-start) ·
[How it works](#how-it-works) ·
[Commands](#command-reference) ·
[Performance](#performance-and-gpu) ·
[Contributing](#contributing)

</div>

> [!WARNING]
> `pi-casso` is a serious implementation of a fundamentally unserious idea.
> It searches actual digits, stores real progress, resumes honestly, and still
> provides no practical benefit whatsoever.

## Terminal UI

<p align="center">
  <img src="docs/assets/tui-hunt.png"
       alt="The HUNT tab during a live search: throughput, best score, pi cache growth, the target mask, and the raw pi digits of the best match"
       width="960">
  <br>
  <em>HUNT, mid-search — the emerged digit highlighted where it lands on the target</em>
</p>

<p align="center">
  <img src="docs/assets/tui-wizard.png"
       alt="The new-search wizard: essential fields, a green START SEARCH button, and advanced options below a separator"
       width="960">
  <br>
  <em>The wizard — essentials, then START SEARCH, then everything with a default</em>
</p>

Five tabs, a command palette, and a search that keeps running underneath all of
them. Humanity demanded dashboards even for searching π for a tiny Arch logo, so
here we are.

| Tab | What lives there |
| --- | --- |
| **HUNT** | The new-search wizard, or the live dashboard once a hunt is running |
| **RUNS** | Every saved run, with its target, best match and history alongside |
| **GALLERY** | Built-in templates and their previews |
| **DATA** | π cache status and digit-file import |
| **SETTINGS** | Theme, glyphs and search defaults, written to `config.toml` |

`1`–`5` or `Tab` switch tabs, `ctrl+p` opens a fuzzy command palette listing
every action available right where you are, and `?` (or `F1`) shows the keys for
the current view. Both are generated from the same registry the keys resolve
against, so they cannot describe a binding that no longer exists.

Bitmaps switch to half-block rendering when a panel is too short for one row per
pixel, which is how a 24×24 emergence canvas fits next to its target instead of
scrolling. Three themes ship — `dark`, `light` and `mono` — and Unicode glyphs
can be turned off independently of colour.

## Why

`pi-casso` asks one needlessly expensive question:

> Can a slice of π be interpreted as something resembling ASCII art?

The answer is usually “not very well,” but the search is reproducible,
resumable, and honest:

- digits are read from real local sources or generated into a persistent cache;
- failed searches remain failed searches rather than inspirational statistics;
- the best match and improvement history are stored in SQLite;
- endless hunts can continue until the machine, the user, or the universe gives up.

Perfect matches for large images are astronomically unlikely. This is not a bug.
It is the project’s only reliable source of long-term engagement.

## At a glance

| | |
| --- | --- |
| **Language** | Rust 2024 |
| **Interfaces** | Interactive TUI, plain terminal output, JSON export |
| **Input** | Generated π cache, local digit files, embedded demo sample |
| **Targets** | Built-in `arch` and `pi` templates or custom ASCII art |
| **Matching** | Emergence scoring and legacy threshold modes |
| **Persistence** | SQLite runs plus a reusable local π cache |
| **Acceleration** | Multithreaded CPU, optional WGPU, and capability-gated CUDA |
| **Usefulness** | Statistically difficult to detect |

## Features

- Tabbed terminal UI with a command palette, live metrics and three themes
- Built-in `arch` and `pi` ASCII-art templates
- Custom `.txt` artwork
- `8x8`, `12x12`, `16x16`, and custom target sizes
- Real generated digits of π with a persistent local cache
- Finite searches over local digit files
- Resumable runs and periodic checkpoints
- Best-match improvement history
- SQLite state under `$XDG_DATA_HOME/pi-casso/pi-casso.db`, in WAL mode so a
  running hunt and a `pi-casso list` never lock each other out
- Persistent preferences in `$XDG_CONFIG_HOME/pi-casso/config.toml`
- CPU resource and thermal controls
- Optional GPU emergence scoring through WGPU, with a capability-gated native
  CUDA path when a compatible PTX handoff is available
- Plain output for scripts and servers
- JSON export
- Stress testing, because the joke apparently needed benchmarks

## Quick start

### Build

From the repository
```sh
cargo build --release
./target/release/pi-casso
```

Alternatively, install it locally:

```sh
git clone https://github.com/Sir-Starch/pi-casso.git
cd pi-casso
cargo install --path .
pi-casso
```

### Launch the TUI

```sh
./target/release/pi-casso
```

New interactive searches default to a `12x12` target inside an endless
generated-π workflow. No separate digit file is required.

### Start an endless hunt directly

```sh
pi-casso hunt --template arch --name arch-eternal --infinite
```

Resume it later:

```sh
pi-casso resume arch-eternal
```

To resume with an explicit portable CPU choice and a bounded verification run:

```sh
pi-casso resume arch-eternal \
  --backend cpu --gpu off \
  --show-metrics
```

### Run a bounded file search

```sh
pi-casso start \
  --template arch \
  --name arch-scan \
  --pi-file ./pi-digits.txt \
  --limit 1000000 \
  --no-tui
```

## How it works

The default matcher is `emergence`.

The target artwork becomes a binary shape mask:

- filled target pixels define the desired shape;
- empty pixels are background and mostly treated as “don’t care”;
- π remains a canvas of raw decimal digits rather than a thresholded bitmap.

For each candidate canvas, `pi-casso` tests every digit from `0` to `9` and
every target placement. A candidate scores well when one repeated digit covers
the target shape without leaking too much into its local background:

```text
coverage_density = coverage²
contrast = max(0, (coverage − leakage) / max(1 − leakage, ε))
cleanliness = 1 − leakage

score =
    0.70 × coverage_density
  + 0.20 × contrast
  + 0.10 × cleanliness
```

- **coverage** measures how completely the target shape is covered by one repeated digit.
- **leakage** measures how much that digit spills into the background.
- **contrast** rewards a noticeable difference between the shape and its background.
- **squared coverage (density)** penalizes incomplete shapes more heavily.

*(Special case: if `coverage == 1.0` and `leakage == 0.0`, the score is exactly `1.0`.)*

The target and digit canvas have separate dimensions. The default endless
search places a `12x12` target inside a `24x24` canvas:

```sh
pi-casso hunt --template arch --name arch-wall --infinite

pi-casso start \
  --template pi \
  --name pi-wall \
  --match-mode emergence \
  --mode 12x12 \
  --canvas-width 24 \
  --canvas-height 24 \
  --pi-file ./pi-digits.txt
```

### Legacy threshold matching

Threshold mode converts digits into a binary bitmap:

1. normalize the target art;
2. read a same-sized sliding window of π digits;
3. treat digits below the threshold as empty;
4. treat digits at or above the threshold as filled;
5. calculate `matching pixels / total pixels`.

With the default threshold of `5`, digits `0–4` are empty and `5–9` are filled:

```sh
pi-casso start \
  --template pi \
  --name pi-threshold \
  --match-mode threshold \
  --threshold 5 \
  --pi-file ./pi-digits.txt
```

Use `--invert` to compare against the inverted bitmap as well.

<details>
<summary><h2>Pi sources</h2></summary>

### Generated cache

Interactive and `hunt --infinite` searches use a persistent local π cache. If a
search catches up with the generator, it waits for more digits and resumes
automatically.

```sh
pi-casso pi generate --digits 1000000
pi-casso pi cache-info
pi-casso pi info
```

For larger generation jobs, `pi-casso` can invoke
[y-cruncher](https://www.numberworld.org/y-cruncher/):

```sh
pi-casso pi generate \
  --digits 1000000000 \
  --generator-backend y-cruncher \
  --y-cruncher-path /path/to/y-cruncher \
  --workers 32
```

Generated output is normalized and appended to:

```text
$XDG_DATA_HOME/pi-casso/pi-cache.txt
```

Temporary y-cruncher output is removed after import. With
`--generator-backend auto`, y-cruncher is used when available; otherwise
`pi-casso` falls back to its built-in CPU generator.

`--generator-backend y-cruncher` is different: it is an explicit request. If
the executable cannot be validated, starts no generation work and returns a
typed unavailable result instead of silently changing generators. Use `auto`
when CPU fallback is acceptable, and inspect JSON output for
`selected_variant`, `unavailable_backends`, `fallback`, and
`fallback_reason`. A generator result is only comparable when its selected
variant and workload are the same; an isolated digits/second number is not an
end-to-end search result.

### Local digit files

Finite searches accept text files containing digits of π. ASCII whitespace is
ignored:

```text
3141592653 5897932384
6264338327 9502884197
```

Other characters are rejected by default, including the decimal point in
`3.1415`. This catches accidental formatting rather than silently shifting every
offset.

Use `--allow-decimal-prefix` only for files intentionally beginning with `3.`:

```sh
pi-casso start \
  --template arch \
  --name arch-file \
  --pi-file ./pi-digits.txt \
  --allow-decimal-prefix
```

Import an existing file into the generated cache:

```sh
pi-casso pi import ./pi-digits.txt
pi-casso pi import ./pi-with-prefix.txt --allow-decimal-prefix
```

### Demo sample

When a finite `start` command has no `--pi-file`, `pi-casso` uses a tiny embedded
sample and prints a warning. It is intended for smoke tests and previews, not
for finding enlightenment in the first few digits.

</details>

## Templates and custom art

List built-in templates:

```sh
pi-casso templates
```

Preview one:

```sh
pi-casso preview --template arch
pi-casso preview --template arch --mode 12x12
```

Built-in sources:

- [Arch Linux artwork](https://archlinux.org/art/)
- [ASCII Pi](https://legend-of-iphoenix.github.io/ascii-pi/)

Preview custom art:

```sh
pi-casso preview --file ./cat.txt --width 16 --height 16
```

By default, spaces are empty and visible characters are filled:

```text
 /\_/\
( o.o )
 > ^ <
```

Explicit character mappings are also supported:

```sh
pi-casso preview --file ./cat.txt --empty " ." --filled "#@"
```

Artwork is trimmed, resized using nearest-neighbor sampling, and padded to
preserve its aspect ratio where possible. Sophisticated image processing would
only make this whole activity harder to defend.

## Command reference

### Search lifecycle

| Command | Purpose |
| --- | --- |
| `pi-casso` | Open the interactive TUI |
| `pi-casso start ...` | Start a finite search |
| `pi-casso hunt ... --infinite` | Start an endless generated-π search |
| `pi-casso resume <name>` | Continue from the latest checkpoint |
| `pi-casso status <name>` | Show saved run status |
| `pi-casso show-best <name>` | Render the current best match |
| `pi-casso history <name>` | Show best-match improvements |
| `pi-casso export <name> --format json` | Export a run |
| `pi-casso delete <name>` | Delete a saved run |

### Useful examples

```sh
pi-casso start --template arch --name arch-hunt --pi-file ./pi-digits.txt
pi-casso start --template arch --name arch-small --mode 8x8 --pi-file ./pi-digits.txt

pi-casso start \
  --file ./cat.txt \
  --name cat-hunt \
  --width 16 \
  --height 16 \
  --pi-file ./pi-digits.txt \
  --limit 1000000 \
  --no-tui
```

### Search limits

A search normally stops when:

- a 100% match is found;
- its finite digit source is exhausted;
- the user interrupts it.

Bound it explicitly with either scanned-window count or an exclusive maximum
offset:

```sh
pi-casso start \
  --template arch \
  --name arch-scan \
  --pi-file ./pi-digits.txt \
  --limit 500000

pi-casso start \
  --template pi \
  --name pi-scan \
  --pi-file ./pi-digits.txt \
  --max-offset 10000000
```

<details>
<summary><h2>Performance and GPU</h2></summary>

The default `balanced` profile controls worker count, chunk size, throttling,
checkpoint cadence, memory budget, and TUI refresh rate without changing search
correctness.

| Profile | Intended use |
| --- | --- |
| `eco` | Laptops, weak hardware, and background searches |
| `balanced` | Moderate CPU use and normal UI refresh |
| `performance` | Strong desktops and faster scanning |
| `max` | All-out search-flavoured stress test |
| `custom` | Balanced defaults with manual overrides |

Profiles are resource policies, not performance promises. They scale worker
counts from the available logical CPUs and may be further limited by the
thermal, battery, memory, queue, backend, and device choices. `eco` deliberately
uses a zero GPU duty policy; `max` permits all available CPU workers and the
largest configured GPU in-flight budget. Neither profile proves that the host
has a usable GPU or that it will be faster.

```sh
pi-casso hunt --template arch --name arch-eco --infinite --profile eco

pi-casso hunt \
  --template arch \
  --name arch-fast \
  --infinite \
  --profile performance \
  --backend auto

pi-casso hunt \
  --template arch \
  --name arch-custom \
  --infinite \
  --profile custom \
  --cpu-workers 8 \
  --chunk-size 2000000 \
  --cpu-utilization 90
```

`max` and stress-test modes require acknowledgement. In non-interactive use,
pass `--yes` or `--force`:

```sh
pi-casso hunt --template arch --name arch-max --infinite --profile max --yes
pi-casso stress-test --stress-target cpu --stress-duration 60 --yes
```

### Resource controls

```text
--cpu-workers <n>
--cpu-utilization <1-100>
--gpu-utilization <1-100>
--chunk-size <n>
--queue-depth <n>
--memory-limit-mb <n>
--ui-refresh-ms <n>
--max-fps <n>
--thermal-mode quiet|normal|aggressive
--background-yield-ms <n>
--pause-when-on-battery
```

`--gpu-utilization` is a bounded submission-duty policy, not a reading of GPU
hardware utilization. `100` permits the configured in-flight budget, and lower
values can add measured policy wait between submissions. The `eco` profile's
zero duty policy disables GPU submission duty; the current explicit CLI override
accepts `1..=100`. Queue depth and memory limit are hard resource controls: a
run that cannot reserve its bounded working set reports a resource error before
opening the digit source.

### Resume and overrides

```sh
# Continue from the recorded checkpoint.
pi-casso resume arch-eternal --show-metrics

# Make the resumed run portable by requiring CPU execution.
pi-casso resume arch-eternal --backend cpu --gpu off --cpu-workers 4
```

Resume continues from `current_offset`, never from the best-match offset. Use
`--show-metrics` to confirm the profile, effective backend, queue, memory, and
stop/wait fields selected for the resumed run. The current host is probed before
accelerated work: if a requested backend is unavailable, inspect the typed
capability result and choose CPU or `auto` intentionally.

### GPU backend

The default emergence matcher can evaluate candidate `(window, placement)`
scores in a WGPU compute shader. Adapter selection happens at runtime, so the
implementation is not tied to a particular GPU model or vendor.

- `--backend auto` keeps small searches on the optimized CPU path and switches
  to GPU for larger emergence or stress workloads;
- `--backend gpu` or `--gpu on` requests GPU explicitly;
- initialization failures fall back cleanly to CPU;
- legacy threshold and exact modes remain CPU-only because transfer overhead
  usually outweighs the work;
- decimal π generation remains CPU-oriented.

```sh
pi-casso gpu info
pi-casso benchmark --template arch --seconds 10 --profile eco
pi-casso benchmark --template arch --seconds 10 --profile performance --backend auto
```

Check capability before treating a GPU result as evidence:

```sh
pi-casso --json gpu info
pi-casso --json benchmark \
  --template arch --source-mode finite --cache-state cold \
  --work-windows 65536 --profile performance --backend auto
```

`auto` can select CPU for a small workload or after an unavailable accelerator
preflight. An explicit `--backend gpu --gpu on` or `--backend cuda --gpu on`
requires that backend to pass preflight; its structured result names the
capability reason instead of presenting a CPU fallback as GPU work. JSON
reports keep `requested_backend` separate from `resolved_backend`, and include
the selected device, backend candidates, fallback state, and fault status.

CUDA is capability-gated rather than inferred from a device name or the
presence of a PTX file. `gpu info` reports the versioned capability state,
driver/device observation, compute capability, and kernel-load status. The
native CUDA path requires the compiled CUDA feature, a loadable `emergence`
kernel, and a PTX handoff that the CUDA driver accepts for the selected device;
the runtime does not require one exact GPU architecture. Unavailable
drivers/devices, an incompatible PTX handoff, missing/invalid handoff
artifacts, or a kernel load failure are explicit unavailable reasons. On
machines without a compatible CUDA handoff, use WGPU or `--backend auto`.

### Measured throughput

The following is a local reference measurement, not a hardware guarantee. It
uses the same `arch` workload for 1,048,576 windows, a 262,144-window
accelerator chunk, and queue depth 4. The GPU numbers were measured on an RTX
4080 SUPER only as the validation host; other GPUs should be benchmarked with
the same command and workload.

| Backend | Throughput |
| --- | ---: |
| CPU | 65.4K windows/s median |
| WGPU validation host | 3.31M windows/s median, 3.17M p95 |

Warm-cache end-to-end time was about 0.313 seconds. The controlled WGPU
baseline was 57.9K windows/s; an earlier small-buffer run measured about
7.9K windows/s. The optimized path submits 16 batches with up to four in
flight and had about 1 ms of queue wait on this workload.

Growing-cache searches have a different limit: when the search reaches the
current end of the cache, throughput is governed by Pi generation. In the
same local validation, growing end-to-end throughput was 48.3K windows/s with
about 956K generated digits/s and 34.6 seconds of generation wait. The producer
prefetches the next range while the accelerator scans, and the TUI reports
that wait and generator rate instead of presenting it as a silent search hang.

GitHub Actions intentionally compiles all test targets with `--no-run`; it does
not generate Pi digits, execute searches, or run hardware benchmarks. It still
checks formatting, Clippy, MSRV compatibility, release builds, and compilation
of all-feature/all-target test binaries. Run the full runtime suite and GPU
benchmarks locally on the target hardware.

### Benchmark and metric interpretation

Benchmark JSON is an audit record, not a leaderboard line. Use a fixed source
mode, cache state, work-window budget, profile, and backend when comparing
runs. With repetitions, the top-level report supplies median and p95 summaries
and retains the individual raw repetitions.

```sh
pi-casso --json benchmark \
  --template arch --source-mode growing --cache-state cold \
  --work-windows 65536 --repetitions 5 --warmup 1 \
  --profile performance --backend cpu --gpu off --show-metrics
```

Read the result in this order:

- `status`, `reason`, `skip_reason`, `stop_reason`, and `workload_id` say
  whether the requested workload actually ran and why it ended.
- `requested_backend`, `resolved_backend`, `backend_device`, `fallback`, and
  `fallback_reason` identify the route actually used.
- `source_digits_per_second` measures source digits read per elapsed wall time.
  `logical_window_digits_per_second` is window-expanded logical work and is
  intentionally a different metric; do not compare the two as substitutes.
- `stage_timings`, `waits`, generator/cache counters, queue occupancy, and
  logical memory peak explain where time or bounded capacity went.
- GPU submission/overlap fields describe the scheduler. A duty-policy percent,
  `active_submission_ratio`, or a `test_only_mock` result is not measured
  hardware utilization or hardware throughput.

For generator comparisons, also require the reported generator backend,
`selected_variant`, and executable hash to match (or deliberately differ) and
compare end-to-end search waits as well as generated digits per second.

### Troubleshooting runtime selection

| Symptom | What to check | Safe next step |
| --- | --- | --- |
| GPU request is unavailable | `pi-casso --json gpu info` and the report `reason` | Use `--backend cpu --gpu off`, or use `--backend auto` when fallback is acceptable. |
| CUDA request is unavailable | CUDA capability state, compute capability, and `kernel_load_status` | Do not treat PTX presence as proof; use CPU/auto unless preflight is `preflight_ok`. |
| Auto chose CPU | `backend_candidates`, workload budget, and capability reason | Increase only a deliberately chosen benchmark workload; CPU selection is valid evidence. |
| y-cruncher request failed | `unavailable_backends` and the typed reason | Fix the executable/import issue, or choose `--generator-backend auto` for CPU fallback. |
| Resume cannot use saved accelerator | Requested versus resolved backend/device and the error reason | Resume with explicit `--backend auto`, or force `--backend cpu --gpu off`; progress remains checkpointed. |
| Numbers look unexpectedly high | Metric name, source/cache state, warm-up/repetitions, and mock flag | Compare only identical workload identities and never present logical or mock metrics as hardware throughput. |

### TUI controls

Global, everywhere:

| Key | Action |
| --- | --- |
| `1`–`5`, `Tab` / `Shift+Tab` | Switch tabs |
| `ctrl+p` | Command palette |
| `?` or `F1` | Help for the current view |
| `ctrl+q` / `ctrl+c` | Quit, checkpointing any live search first |

In the wizard:

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move between fields |
| `←` / `→` / `Space` | Adjust the focused choice, number or toggle |
| `Enter` | Next field, or start when the Start row has focus |
| `F9` | Start immediately, from any field |

**START SEARCH** sits directly after the essentials, with everything that has a
sensible default below it under an `advanced` heading — five presses of `Enter`
from a cold start, or one click, or `F9` from wherever you happen to be.

While a hunt is running:

| Key | Action |
| --- | --- |
| `Space` | Pause or resume |
| `p` | Next performance profile |
| `F2` / `F3` / `F4` / `F5` | Select `eco`, `balanced`, `performance`, or `max` directly |
| `+` / `-` | Adjust workers |
| `]` / `[` | Adjust chunk size |
| `g` | Cycle GPU mode |
| `t` | Cycle thermal mode |
| `m` | Toggle metrics |
| `s` | Save a checkpoint now |
| `e` | Export the run to JSON |
| `Esc` | Stop, saving a checkpoint |

Anything with a key also appears in the command palette, along with the actions
that have none. A focused text field takes printable keys before any shortcut
does, so a path containing an `h` is typeable — which it previously was not.

### Mouse

Everything that looks clickable is clickable:

| Target | Click does |
| --- | --- |
| A tab | Switches to it |
| A list row | Selects it |
| A form field | Focuses it — and on a choice or toggle, advances it |
| **START SEARCH** | Starts the hunt |
| A key cap in the bottom bar | Runs that action |
| A command in the palette | Runs it; clicking outside dismisses it |
| **Delete** / **Cancel** in a confirmation | Answers it |

Clicking a value changes the value, because that is what a value that looks like
a button should do. The wheel scrolls lists and moves focus through forms.

Filled key caps in the bottom bar are buttons; dim ones are descriptions of how
the keyboard behaves there and do nothing on a click. Nothing in the interface
is keyboard-only.

### What the dashboard tells you

Throughput is reported **over the last few seconds**, with the session average as
small print beneath it. A cumulative average barely moves when a search slows
down and never visibly recovers, which makes it useless for the question people
actually ask — so switching profile with `p` shows up in the headline number and
the sparkline right away.

The π cache metric distinguishes the two reasons a search might pause. When the
search outruns the generator it reads `+69.0K/s, 4.3M to go` — digits are being
computed and here is how fast. Only a genuinely stopped producer reads as
`waiting for digits`. The old interface called both of them "waiting for more
pi", which made productive work look like a hang.

The best match is shown as the raw π digits of the winning window, with the
emerged digit highlighted where it lands on the target, above a line giving that
digit, its position, coverage and leakage. Apparently even pointlessness needs
observability.

</details>

## Configuration

Preferences live in a TOML file the SETTINGS tab writes for you:

```text
$XDG_CONFIG_HOME/pi-casso/config.toml
```

```toml
[appearance]
theme = "dark"      # dark | light | mono
unicode = true      # false falls back to pure ASCII
max_fps = 30        # upper bound on TUI redraws

[search]
profile = "balanced"
match_mode = "emergence"
width = 12
height = 12
canvas_width = 24
canvas_height = 24
threshold = 5
template = "arch"

[paths]
# export_dir = "/somewhere/else"   # defaults to <data dir>/exports
```

Every key is optional; anything missing falls back to the built-in default, and
an unreadable file is reported as a warning rather than being fatal. Unknown
keys are rejected, because a silently ignored typo is worse than a complaint.

`--no-color`, `NO_COLOR`, and a non-terminal stdout each disable colour without
discarding your glyph preference. `PI_CASSO_DATA_DIR` relocates the database,
the π cache, exports, and the config together — which is how the test suite
keeps out of your real state.

## Plain output example

```text
[00:01:42] offset=182400000 scanned=182399744 speed=2.1M/s best=87.50% at offset=91441203

new best: 91.80% at offset 918273645 after 918273646 scanned windows
....##..........
...####.........
..#####.........
.###..###.......
###.....##......

finished: limit reached (offset=1000000, scanned=1000000, best=91.80%)
progress saved. Resume will continue from the current offset.
```

<details>
<summary><h2>Storage and architecture</h2></summary>

Digit input is abstracted behind `DigitSource`. File, demo, cache, and generated
sources can share the same search pipeline.

Progress is stored in SQLite:

```text
$XDG_DATA_HOME/pi-casso/pi-casso.db
```

`resume` continues from `current_offset`, never from the best-match offset.
Best-match improvements are written immediately, while periodic checkpoints
persist scan progress.

At a high level:

```text
ASCII target
    ↓ normalization
binary target mask
    ↓
π digit source → sliding candidate canvases
    ↓
CPU or GPU matcher
    ↓
best-match updates + checkpoints
    ↓
TUI / plain output / JSON export
```

</details>

## Project status

`pi-casso` is an experimental meme project. It may change rapidly, consume
unreasonable amounts of compute, and produce screenshots far more valuable than
its results.

The project does make several things deliberately non-joke-grade:

- input validation;
- deterministic matching semantics;
- honest offsets and scanned ranges;
- persistent checkpoints;
- clean CPU fallback;
- reproducible exports.

There is currently no promise of stable CLI compatibility, package
availability, or any eventual moment at which the search becomes sensible.

## Contributing

Bug reports, performance measurements, platform fixes, and improvements to the
TUI are welcome.

Before submitting changes:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Changes that invent digits, fabricate search results, or make the project
genuinely useful will be reviewed with appropriate suspicion.

## License

Licensed under either:

- [Apache License 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.

Unless explicitly stated otherwise, contributions intentionally submitted for
inclusion are dual-licensed under the same terms without additional conditions.
