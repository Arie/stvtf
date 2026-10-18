#!/usr/bin/env python3
"""Cut a TF2 .dem after a given tick, for small test fixtures.

    tools/cut_demo.py IN.dem OUT.dem --tick N

N is a demo tick, counted from the start of the recording (not the server tick).

Copies the header and every packet up to demo tick N, then ends the demo with a stop
packet, and fixes up the header's tick, frame and duration counts. A demo is a
stream of deltas, so only its end can be cut, never its start or middle.

Packet layout (demo protocol 3): u8 command, i32 tick, then by command:
  1 signon / 2 packet: 76 bytes cmd info, i32 in seq, i32 out seq, i32 length, data
  3 sync tick:         nothing
  4 console command:   i32 length, data
  5 user command:      i32 sequence, i32 length, data
  6 data tables:       i32 length, data
  7 stop:              end of the demo
  8 string tables:     i32 length, data
"""

import argparse
import struct
import sys

HEADER = 1072
# Offsets of the header fields after the four 260-byte strings.
DURATION, TICKS, FRAMES = 1056, 1060, 1064
CMD_INFO = 76
STOP = 7


def packets(data):
    """Yields (start, end, command, tick) for each packet after the header."""
    pos = HEADER
    # Some demos end without a stop packet, in a few bytes of trailer.
    while pos + 5 <= len(data):
        start = pos
        cmd, tick = struct.unpack_from("<Bi", data, pos)
        pos += 5
        if cmd in (1, 2):
            pos += CMD_INFO + 8
            (n,) = struct.unpack_from("<i", data, pos)
            pos += 4 + n
        elif cmd == 3:
            pass
        elif cmd in (4, 6, 8):
            (n,) = struct.unpack_from("<i", data, pos)
            pos += 4 + n
        elif cmd == 5:
            (n,) = struct.unpack_from("<i", data, pos + 4)
            pos += 8 + n
        elif cmd == STOP:
            return
        else:
            sys.exit(f"unknown demo command {cmd} at byte {start}")
        yield start, pos, cmd, tick


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("input")
    ap.add_argument("output")
    ap.add_argument("--tick", type=int, required=True, help="last demo tick to keep (counted from the start of the recording)")
    args = ap.parse_args()

    with open(args.input, "rb") as f:
        data = f.read()
    if data[:8] != b"HL2DEMO\0":
        sys.exit("not a Source demo")

    out = bytearray(data[:HEADER])
    first = last = None
    frames = 0
    for start, end, cmd, tick in packets(data):
        # Signon packets carry tick 0; only game ticks count towards the cut.
        if cmd != 1 and tick > args.tick:
            break
        out += data[start:end]
        if cmd == 2:
            frames += 1
            first = tick if first is None else first
            last = tick
    if first is None:
        sys.exit("no game packets before that tick")
    out += struct.pack("<Bi", STOP, last)
    out += b"\0"  # the engine writes one padding byte after the stop command

    (interval_ticks,) = struct.unpack_from("<i", data, TICKS)
    (interval_secs,) = struct.unpack_from("<f", data, DURATION)
    per_tick = interval_secs / interval_ticks if interval_ticks else 0.015
    struct.pack_into("<f", out, DURATION, (last - first) * per_tick)
    struct.pack_into("<i", out, TICKS, last - first)
    struct.pack_into("<i", out, FRAMES, frames)

    with open(args.output, "wb") as f:
        f.write(out)
    print(f"kept ticks {first}..{last}, {frames} frames, {len(out) >> 20} MiB", file=sys.stderr)


if __name__ == "__main__":
    main()
