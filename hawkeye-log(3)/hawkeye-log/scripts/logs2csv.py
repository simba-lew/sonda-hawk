#!/usr/bin/env python3
"""Turns log files into a CSV of telemetry, one row per telemetry packet.

    python3 scripts/logs2csv.py logs/panel-*.log L0001.TXT -o flight.csv

Reads both kinds of log, found by the shape of each line (a file can be either, or mixed):
  ground (logs/panel-<date>-<time>.log): `14:47:49.341 INFO - telemetry #35 fix ... RSSI -43 dBm, SNR 13 dB`
      the balloon's telemetry as the ground station received it (with RSSI and SNR)
  balloon (L0001.TXT from the SD card): `[   412] INFO - telemetry #1 sent: fix ... sats 8 | ...`
      what the balloon sent (with the satellite count)

The `time` column is a date and time for ground rows (the date comes from the file name, and moves
on at midnight) and empty for balloon rows, which only have `uptime_ms`, the time since that boot.
Without -o the CSV goes to the screen.
"""

import argparse
import csv
import datetime
import os
import re
import sys

COLUMNS = [
    "source", "file", "time", "uptime_ms", "seq", "fix", "lat", "lon", "alt_m", "sats",
    "pressure_hpa", "temp_c", "humidity_pct", "rssi_dbm", "snr_db",
]

NUM = r"-?\d+(?:\.\d+)?"
# the part after "telemetry #" is the same up to the air data; the rest differs
HEAD = (rf"telemetry #(?P<seq>\d+)(?: sent:)? fix (?P<fix>true|false) (?P<lat>{NUM}) (?P<lon>{NUM}) "
        rf"alt (?P<alt>{NUM}) m(?:, sats (?P<sats>\d+))? \| ")
AIR = rf"(?P<press>{NUM}) hPa (?P<temp>{NUM}) C (?P<hum>{NUM}) %RH"
BALLOON = re.compile(rf"^\[\s*(?P<ms>\d+)\]\s.*?{HEAD}{AIR}")
GROUND = re.compile(rf"^(?P<t>\d\d:\d\d:\d\d\.\d{{3}})\s*.*?{HEAD}{AIR} \| RSSI (?P<rssi>-?\d+) dBm, SNR (?P<snr>-?\d+) dB")
FILE_DATE = re.compile(r"(\d{8})-\d{6}")


def rows(path):
    """The telemetry rows of one log file."""
    name = os.path.basename(path)
    m = FILE_DATE.search(name)
    day = datetime.datetime.strptime(m.group(1), "%Y%m%d").date() if m else None
    last = None
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\r\n")
            if (m := BALLOON.match(line)):
                src = "balloon"
                row = {"uptime_ms": m["ms"], "sats": m["sats"] or ""}
            elif (m := GROUND.match(line)):
                src = "ground"
                t = datetime.datetime.strptime(m["t"], "%H:%M:%S.%f").time()
                if last is not None and t < last and day is not None:
                    day += datetime.timedelta(days=1)  # past midnight
                last = t
                stamp = datetime.datetime.combine(day, t).isoformat(timespec="milliseconds") if day else m["t"]
                row = {"time": stamp, "rssi_dbm": m["rssi"], "snr_db": m["snr"]}
            else:
                continue
            row.update({
                "source": src, "file": name, "seq": m["seq"], "fix": m["fix"], "lat": m["lat"], "lon": m["lon"],
                "alt_m": m["alt"],
                "pressure_hpa": m["press"], "temp_c": m["temp"], "humidity_pct": m["hum"],
            })
            yield row


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("logs", nargs="+", help="ground (panel) and/or balloon (SD card) log files")
    ap.add_argument("-o", "--output", help="CSV file to write (default: the screen)")
    args = ap.parse_args()

    out = open(args.output, "w", newline="", encoding="utf-8") if args.output else sys.stdout
    writer = csv.DictWriter(out, fieldnames=COLUMNS, restval="", lineterminator="\n")
    writer.writeheader()
    count = 0
    for path in args.logs:
        n = 0
        for row in rows(path):
            writer.writerow(row)
            n += 1
        print(f"{path}: {n} telemetry rows", file=sys.stderr)
        count += n
    if args.output:
        out.close()
    if count == 0:
        sys.exit("no telemetry lines found")


if __name__ == "__main__":
    main()
