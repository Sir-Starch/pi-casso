#!/usr/bin/env bash
set -euo pipefail

output=""
digits=""
while (($#)); do
  case "$1" in
    --output) output=${2:?}; shift 2 ;;
    --digits) digits=${2:?}; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
test -n "$output" && [[ $digits =~ ^[1-9][0-9]*$ ]] || {
  echo "usage: create-ycruncher-fixture.sh --output PATH --digits N" >&2
  exit 2
}
[[ ! -e $output && ! -L $output ]] || { echo "fixture output already exists" >&2; exit 2; }
mkdir -p "$(dirname "$output")"
temporary=$(mktemp "$(dirname "$output")/.y-cruncher-fixture.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
{
  printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail'
  printf 'fixture_max_digits=%q\n' "$digits"
  cat <<'FIXTURE'
if (($# != 13 && $# != 14)); then
  exit 2
fi
[[ $1 == pause:-2 && $2 == skip-warnings && $3 == colors:0 &&
   $4 == custom && $5 == pi && $6 =~ ^-dec:[0-9]+$ &&
   $7 == -hex:0 && $8 == -od:1 && $9 == -compress:0 &&
   ${10} == -verify:0 && ${11} == -mode:ram && ${12} == -o &&
   -n ${13} ]] || exit 2
if (($# == 14)); then
  [[ ${14} =~ ^-TD:[1-9][0-9]*$ ]] || exit 2
fi
decimal_places=${6#-dec:}
[[ ${#decimal_places} -le 9 ]] || exit 2
requested_digits=$((10#$decimal_places + 1))
((requested_digits <= fixture_max_digits)) || exit 2
output_dir=${13}
if command -v cygpath >/dev/null 2>&1; then
  output_dir=$(cygpath -u "$output_dir")
fi
mkdir -p "$output_dir/results"
destination="$output_dir/results/pi-decimal.txt"
temporary=$(mktemp "$output_dir/results/.pi-decimal.XXXXXX")
trap 'rm -f -- "$temporary"' EXIT
python3 - "$temporary" "$requested_digits" <<'PY'
import math
import os
import sys

destination = sys.argv[1]
digits = int(sys.argv[2])
if hasattr(sys, "set_int_max_str_digits"):
    sys.set_int_max_str_digits(0)
C3_OVER_24 = 640320**3 // 24

def binary_split(a: int, b: int) -> tuple[int, int, int]:
    if b - a == 1:
        if a == 0:
            p = q = 1
        else:
            p = (6 * a - 5) * (2 * a - 1) * (6 * a - 1)
            q = a * a * a * C3_OVER_24
        t = p * (13591409 + 545140134 * a)
        if a & 1:
            t = -t
        return p, q, t
    middle = (a + b) // 2
    p1, q1, t1 = binary_split(a, middle)
    p2, q2, t2 = binary_split(middle, b)
    return p1 * p2, q1 * q2, t1 * q2 + p1 * t2

guard_digits = 20
scale = 10 ** (digits + guard_digits)
terms = (digits + guard_digits) // 14 + 2
_, q, t = binary_split(0, terms)
sqrt_10005 = math.isqrt(10005 * scale * scale)
scaled_pi = (q * 426880 * sqrt_10005) // t
text = str(scaled_pi)[:digits]
if len(text) != digits or not text.startswith("3141592653589793"):
    raise SystemExit(1)
with open(destination, "w", encoding="ascii", newline="\n") as output:
    output.write(text)
    output.write("\n")
PY
mv -- "$temporary" "$destination"
trap - EXIT
FIXTURE
} > "$temporary"
chmod 755 "$temporary"
mv -- "$temporary" "$output"
trap - EXIT
