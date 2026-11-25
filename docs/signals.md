# Signals: sequences of numbers

A token sequence holds symbols. A meter reading, a temperature or a
latency is a number, and turning numbers into symbols either loses them
or costs a symbol per distinct value. `kute signals` stores a sequence
of numbers as numbers.

    kute signals pack voltage.txt --output voltage.kts
    kute signals pack voltage.txt --output voltage.kts --max-error 0.05
    kute signals pack log.csv --column 5 --delimiter ';' --skip-lines 1 --output v.kts
    kute signals unpack voltage.kts --output back.txt
    kute signals inspect voltage.kts

Stability: **experimental**. The container format (`KUTE-SG1`) and the
commands may change. Library: the `kute-signals` crate.

## The contract

A signal is a sequence of 64-bit floating point values. There are two
contracts, and the header of every container says which it was made
under.

* **exact** (the default). Every value comes back with the same bits:
  the same number, and also the same NaN, the same infinity, the same
  sign on zero.
* **bounded** (`--max-error E`). Every finite value comes back within `E`
  of the original, in absolute terms. Values that are not finite come
  back exactly. `E` is in the units of the signal.

The bound is checked, not assumed. While packing, each value is rebuilt
the way a reader will rebuild it; a value that would miss the bound is
kept exactly instead. Before `pack` returns it reads its own container
back and compares every value against the contract, and writes nothing
if one fails. The header records the largest error any value actually
has (`max_error_observed`), which is at most `E` and often less.

`unpack` recomputes a SHA-256 over the values it produced and refuses the
container if it differs from the one in the header. A damaged or altered
container gives an error, not other numbers.

The hash covers the values and not what the header says about them, so
the header's statements are checked against each other: a container in
steps that calls itself exact, a bound of zero, an observed error above
the bound, are all refused by `unpack` and `inspect`. What no check can
do is tell a reader what the original values were. A container made by
someone you do not trust can state any bound it likes over values that
were never within it; the header is the maker's word.

What is not kept: the spelling. `234.840` and `234.84` are one value and
`unpack` prints it in the shortest form that reads back as the same
number, with an exponent when the plain form would pass 24 characters.
A missing reading (an empty field, `?`, `NA`, `NaN`) is stored as NaN
and comes back as `NaN`: a file that used both `?` and `NaN` comes back
with one spelling for the two. To keep text byte for byte, this is the
wrong command.

## How it is stored

Standard techniques, none of them new:

1. Find the fewest decimal places (up to nine) at which the values are
   whole numbers of that unit; or, for a bounded container, use steps of
   twice the bound.
2. Take differences between neighbours zero, one or two times, whichever
   stores smallest by trial.
3. Write them as variable-length integers and compress with zstd.
4. Keep beside it, as raw bits with their positions, every value the
   scheme cannot give back: NaN, infinities, a value with no short
   decimal form, a value too large for the step.

If more than one finite value in sixteen has no decimal form, the signal is
stored as its bytes arranged by byte position and compressed: still
exact, and close to what a general compressor gets.

A bounded container is written both ways, in steps and as the values are,
and the smaller is kept. Exact values satisfy any bound, and when the
bound is finer than the signal's own resolution they are the smaller of
the two.

Nothing is predicted beyond the last one or two values. There is no
model of the signal, no transform, no per-segment adaptation.

## Limits

* At most 2^31 values in a container. `unpack --max-values` (default
  2^24) bounds what a container can make a reader allocate; each part's
  size is checked against the number of values before it is inflated.
  Reading can take about 44 bytes of memory a value, not 8, so the
  default allows about 740 MB and the format's limit about 90 GB. Set
  it to what you expect.
* One signal per container. A table is one container per column.
* The bound is absolute. There is no relative bound, and no bound on a
  derived quantity such as a sum or a rate.
* A bounded container is not a contract about shape: a step of the bound
  can turn a slow ramp into a staircase.
* `pack` reads the whole signal into memory, and a bounded pack does the
  work twice.

## Measurement

`benchmarks/signals_power.py`, result in
`benchmarks/results/signals-household-power.json`. Real data: the UCI
"Individual household electric power consumption" set (CC BY 4.0),
2,075,259 readings a minute apart, seven columns, 25,979 readings
missing in each. One run, one data set from one house; the sizes are
deterministic, the timings are not.

Every exact column read back equal to the source values, compared by the
benchmark independently of `kute`. Every bounded column was within its
bound, and the header's stated largest error matched the measured one.

Total over seven columns, bytes:

| | bytes |
|---|---|
| The columns as text, xz -9 | 7,615,428 |
| Eight bytes a value, xz -9 | 7,340,236 |
| Scaled whole numbers, differenced by hand, four bytes each, xz -9 | 6,294,636 |
| The same differences as varints, zstd -19 | 5,987,100 |
| `kute signals`, exact | 5,976,899 |
| Rounded to one digit fewer and printed, xz -9 | 4,033,828 |
| `kute signals`, bounded to one digit fewer | 3,498,994 |

Reading this fairly:

* The third and fourth rows are the same idea done by hand. The fourth
  uses the container's own number format and compressor, so the
  difference between it and the container is what the container itself
  adds in bytes: 10,201 bytes in 6 MB, or 0.2%. On the four columns where both take differences once, the hand-made row is the smaller by a few hundred bytes (it has no header); the container is ahead only on the two sub-meter columns where it chooses not to take differences. In bytes the container adds nothing worth a claim. Both rows store a missing reading as zero
  and so could not give the data back; they are flattered.
* The point is the contract in the file: which mode, what bound, what
  error was incurred, and a hash a reader checks.
* Rivals were xz only. A purpose-built float compressor (Gorilla, Chimp,
  ALP, zfp, SZ) was not run and may well do better. No claim is made
  against them.
* Packing took 0.4 to 1.5 seconds a column and unpacking 0.25 to 0.45,
  including process start and reading and printing text, on one desktop.

Per column, exact, in bits per value: active power 6.33, reactive power
4.38, voltage 8.48, current 2.41, and the three sub-meters 0.24, 0.50
and 0.70 (they are zero most of the time).
