#!/usr/bin/env python3
"""Headless Horus decoder. stdout is a JSON-lines protocol used by panel.py.

Uses exactly the UDP PCM and modem API used by Horus GUI 0.6.x.
Run as a child process so a native modem failure cannot affect USB commands.
"""
import argparse
import json
import math
import socket
import sys
import time
from array import array


def emit(kind, data):
    print(json.dumps({"kind": kind, "data": data}, allow_nan=False), flush=True)


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--port', type=int, default=7355)
    p.add_argument('--sample-rate', type=int, default=48000)
    p.add_argument('--baud', type=int, default=100)
    p.add_argument('--spacing', type=int, default=270)
    args = p.parse_args()
    from horusdemodlib.demod import HorusLib, Mode
    from horusdemodlib.decoder import decode_packet

    def on_frame(frame):
        if frame.data:
            emit('raw', f"CRC {'OK' if frame.crc_pass else 'BAD'} HEX {frame.data.hex().upper()}")
        if not frame.crc_pass or not frame.data:
            return
        try:
            d = decode_packet(frame.data)
            # Decoder metadata includes bytes and struct descriptors. Only send
            # scalar telemetry; preserve the actual received frame separately.
            d = {k: v for k, v in d.items() if v is None or isinstance(v, (str, int, float, bool))}
            d['snr'] = frame.snr if math.isfinite(frame.snr) else None
            d['raw_hex'] = frame.data.hex().upper()
            emit('packet', d)
        except Exception as e:
            emit('warning', str(e))

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        # Deliberately exclusive: two consumers of the same unicast audio port
        # silently splitting packets would produce unreliable decoding.
        sock.bind(('127.0.0.1', args.port))
        sock.settimeout(1)
        with HorusLib(mode=Mode.BINARY, rate=args.baud, tone_spacing=args.spacing,
                      sample_rate=args.sample_rate, callback=on_frame) as modem:
            modem.set_estimator_limits(100, 4000)
            emit('status', f'Dekoder gotowy; UDP 127.0.0.1:{args.port}')
            last_report = 0
            while True:
                try:
                    data = sock.recv(65535)
                except socket.timeout:
                    continue
                if not data or len(data) % 2:
                    continue
                frame = modem.add_samples(data)
                now = time.monotonic()
                if now - last_report >= 1:
                    samples = array('h', data)
                    if sys.byteorder != 'little':
                        samples.byteswap()
                    peak = max(abs(x) for x in samples) / 32768
                    emit('audio', {'received_at': time.time(),
                                   'peak_dbfs': round(20 * math.log10(max(peak, 1e-6)), 1),
                                   'snr': float(frame.snr) if frame and math.isfinite(frame.snr) else None})
                    last_report = now


if __name__ == '__main__':
    try:
        main()
    except Exception as e:
        emit('error', f'{type(e).__name__}: {e}')
        sys.exit(1)
