# pi-casso TUI design system

## 1. Atmosphere & identity

The TUI is a quiet night-lab console: dense enough for a live search, but with
one clear focal point, the target and its best pi match. Its signature is a
mint signal against a charcoal surface, with warm amber reserved for imperfect
or cautionary states.

## 2. Color

| Role | Token | Dark | Light | Usage |
| --- | --- | --- | --- | --- |
| Text / primary | `text` | `#E5E8E0` | `#222D2D` | Values and readable copy |
| Text / secondary | `dim` | `#899796` | `#5A6B6A` | Hints, metadata, labels |
| Border | `border` | `#364544` | `#B7C6C5` | Panel frames and dividers |
| Focus / accent | `accent` | `#67D6AE` | `#148069` | Active tab, headline values |
| Success | `success` | `#75DC9D` | `#1F8952` | Progress, good match, primary action |
| Warning | `warning` | `#EFB15C` | `#A66914` | Paused, leakage, caution |
| Danger | `danger` | `#ED7075` | `#BE363A` | Errors and exhausted sources |
| Canvas | `canvas_bg` | `#121B1C` | `#EDF4F1` | Bitmap surfaces and metric strip |
| Target window | `canvas_target_bg` | `#1C2F2E` | `#DCEDE6` | Search placement context |

Accent is for navigation and primary data only. Status colors are semantic,
not decorative. `mono` keeps the same layout and relies on reverse video and
text modifiers instead of color.

## 3. Typography

The terminal's configured monospace font is the only typeface. The visual scale
is expressed in terminal rows rather than font sizes:

| Level | Shape | Usage |
| --- | --- | --- |
| Brand / active tab | bold, 1 row | `pi-casso` and current tab |
| Metric label | dim uppercase, 1 row | Compact category labels |
| Metric value | bold accent or status color, 1 row | Primary live number |
| Metric hint | dim, 1 row | Average, offset, or cache context |
| Canvas detail | regular/dim | Raw digits and bitmap pixels |

Numbers use the existing grouped/tabular formatting helpers. Labels are short
enough to remain readable in narrow terminals.

## 4. Spacing & layout

The base unit is one terminal cell. Panels use one cell of horizontal padding.
The app shell is ordered as:

1. one-row navigation and status;
2. one-row run context;
3. a three-row live metric strip;
4. the target/best canvas split;
5. a six-row improvement history when height allows;
6. the two-row command bar.

The dashboard gives the best-match canvas the larger column. On short terminals,
history yields first so the live numbers and canvases remain usable.

## 5. Components

### App shell

- **Structure**: tab bar, active content surface, transient toast rail, command bar.
- **Variants**: Hunt wizard, live Hunt dashboard, list/detail tabs.
- **States**: idle, busy, paused, complete, warning, error, too small.
- **Accessibility**: every action remains keyboard-driven; mouse hit areas are
  recorded from the same layout used for drawing.

Toasts reserve a short rail above the command bar while visible, so transient
feedback never obscures the target, match, or history.

### Metric strip

- **Structure**: four equal tonal cells: progress, throughput, best score, and
  pi cache.
- **States**: normal, generating, paused, no match, source exhausted.
- **Layout**: three rows; the value is always above its explanation.

### Canvas panel

- **Structure**: titled bordered panel with a real bitmap or digit stream.
- **Variants**: target canvas, best match / pi stream, preview.
- **States**: empty, populated, comparison-highlighted, error.
- **Layout**: target is centered optically; the best stream gets the wider side.

### Command bar

- **Structure**: top rule, key cap, short action label.
- **States**: available action, descriptive-only hint, truncated at the edge.
- **Accessibility**: caps are clickable where an action exists and every cap has
  a keyboard equivalent.

## 6. Motion & interaction

There is no decorative animation. Live updates are the meaningful motion: the
best score, cache count, status color, and raw match update in place. The
existing frame policy controls update frequency so the terminal stays calm.
Focus, active tabs, key caps, and primary actions use immediate visual state
changes that also work in monochrome mode.

## 7. Depth & surface

Strategy: mixed tonal shift plus restrained borders. Canvas and metric surfaces
use the canvas tone; panels use one-pixel terminal borders; no drop shadows or
fake gradients are introduced. A single accent highlight establishes hierarchy
without turning every line blue.

## 8. Accessibility constraints & accepted debt

### Constraints

- Preserve a readable monochrome fallback.
- Never let a metric or panel overflow the terminal width.
- Keep keyboard navigation and mouse hit regions aligned after layout changes.
- Use status color together with a textual label, never color alone.

### Accepted debt

| Item | Location | Why accepted | Owner / exit |
| --- | --- | --- | --- |
| Terminal font metrics vary by emulator | All TUI screens | Ratatui renders cells, not a portable font | Keep width checks in visual QA |
