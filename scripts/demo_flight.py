"""Synthetic flight for offline UI checks. Never opens USB or sends commands."""
import math
import json
import time


def run_demo(flight):
    flight.radio_status(detail='DEMO — dane syntetyczne; komendy wyłączone')
    start = time.monotonic()
    seq = 0
    while not flight.stop.is_set():
        t = time.monotonic() - start
        for source, shift in [('rs41', 0), ('tbeam', .0001)]:
            packet=dict(source=source, callsign='HAWK' if source == 'rs41' else 'T-Beam',
                seq=seq, fix=True, latitude=52.17818 + t*.000005 + shift,
                longitude=21.18629 + t*.000008 + shift, altitude=106 + 5*t,
                pressure=995.4*math.exp(-5*t/8400), temperature=(21.5-.0065*5*t if source == 'rs41' else 24-0.002*5*t),
                internal_temperature=27 if source == 'rs41' else 24-0.002*5*t,
                custom_temperature=26.4 if source == 'rs41' else None,
                humidity=46+math.sin(t/20)*3, ascent_rate=5, snr=11, satellites=8,
                battery=3.9 if source == 'rs41' else None)
            flight.hub.add_line('[DEMO — dane syntetyczne] ' + json.dumps(packet), source=source)
            flight.ingest(packet)
        seq += 1
        flight.stop.wait(2)
