"""Telemetry normalization, bounded history and online Tawhiri predictions."""
import collections
import csv
import datetime as dt
import json
import math
from pathlib import Path
import queue
import re
import socket
import subprocess
import sys
import threading
import time
import urllib.parse
import urllib.request

TEL = re.compile(r'telemetry #(\d+) fix (true|false) (-?[\d.]+) (-?[\d.]+) alt (-?[\d.]+) m \| (-?[\d.]+) hPa (-?[\d.]+) C (-?[\d.]+) %RH \| RSSI (-?[\d.]+) dBm, SNR (-?[\d.]+) dB')
FIELDS = ['received_at', 'source', 'callsign', 'seq', 'fix', 'latitude', 'longitude',
          'altitude', 'ascent_rate', 'pressure', 'temperature', 'internal_temperature',
          'custom_temperature', 'humidity', 'battery', 'satellites', 'rssi', 'snr', 'payload_time']
PREDICTOR = 'https://api.v2.sondehub.org/tawhiri'


def number(v):
    try:
        n = float(v)
        return n if math.isfinite(n) else None
    except (TypeError, ValueError):
        return None


def position_ok(d):
    lat, lon, alt = (number(d.get(k)) for k in ('latitude', 'longitude', 'altitude'))
    return (d.get('fix') is True and lat is not None and lon is not None and alt is not None
            and -90 <= lat <= 90 and -180 <= lon <= 180 and (lat != 0 or lon != 0)
            and -500 <= alt <= 60000)


def ground_packet(line):
    m = TEL.search(line)
    if not m:
        return None
    v = m.groups()
    return dict(source='tbeam', callsign='T-Beam', seq=int(v[0]), fix=v[1] == 'true',
                latitude=number(v[2]), longitude=number(v[3]), altitude=number(v[4]),
                pressure=number(v[5]), temperature=number(v[6]), humidity=number(v[7]),
                rssi=number(v[8]), snr=number(v[9]))


def horus_packet(d):
    if not isinstance(d, dict):
        raise ValueError('Ramka Horusa nie jest obiektem JSON')
    d = dict(d)
    if d.get('type') == 'PAYLOAD_SUMMARY':
        # Horus UDP uses different field names and -1 as missing sentinel.
        for dest, src in [('satellites', 'sats'), ('battery_voltage', 'batt_voltage')]:
            if dest not in d and number(d.get(src)) is not None and number(d[src]) >= 0:
                d[dest] = d[src]
    out = dict(source='rs41', callsign=str(d.get('callsign', d.get('payload_id', 'RS41'))),
               payload_time=str(d.get('time', '')), seq=number(d.get('sequence_number')))
    aliases = dict(latitude='latitude', longitude='longitude', altitude='altitude',
                   ascent_rate='ascent_rate', pressure='ext_pressure', humidity='ext_humidity',
                   temperature='ext_temperature', internal_temperature='temperature',
                   custom_temperature='temperature_custom_1', battery='battery_voltage',
                   satellites='satellites', snr='snr')
    for dest, src in aliases.items():
        out[dest] = number(d.get(src))
    # A zero satellite count must not turn cached coordinates into a GPS fix.
    out['fix'] = (d.get('fix') is not False and
                  (out['satellites'] is None or out['satellites'] >= 3))
    if out['latitude'] is not None and out['longitude'] is not None:
        out['fix'] = out['fix'] and (out['latitude'] != 0 or out['longitude'] != 0)
    if d.get('raw_hex'):
        out['raw_hex'] = d['raw_hex']
    return out


class Flight:
    def __init__(self, hub, stem, callsign=None):
        self.hub = hub
        self.lock = threading.RLock()
        self.history = {s: collections.deque(maxlen=20000) for s in ('rs41', 'tbeam')}
        self.latest = {}
        self.radio = {'detail': 'Oczekiwanie na dekoder', 'audio': None}
        self.prediction = None
        self.predict_busy = False
        self.predict_error = ''
        self.callsign = callsign
        self.stop = threading.Event()
        self.worker = None
        self.json_path = stem + '.jsonl'
        self.csv_path = stem + '.csv'
        self.json_file = open(self.json_path, 'a', encoding='utf-8', buffering=1)
        self.csv_file = open(self.csv_path, 'w', encoding='utf-8', newline='', buffering=1)
        self.csv = csv.DictWriter(self.csv_file, fieldnames=FIELDS, extrasaction='ignore')
        self.csv.writeheader()

    def event(self, kind, value):
        with self.hub.lock:
            self.hub._publish(kind, value)

    def ingest(self, d):
        source = d['source']
        if source == 'rs41' and self.callsign and d['callsign'] != self.callsign:
            return
        d = dict(d, received_at=time.time())
        with self.lock:
            prev = self.latest.get(source)
            # Same payload counter is a duplicate, except a different payload time.
            if (prev and d.get('seq') is not None and prev.get('seq') == d['seq']
                    and prev.get('callsign') == d.get('callsign')
                    and prev.get('payload_time') == d.get('payload_time')
                    and d['received_at'] - prev['received_at'] < 15):
                return
            valid = position_ok(d)
            d['fix'] = valid
            if valid and d.get('ascent_rate') is None:
                candidates = [p for p in self.history[source] if p['fix'] and
                              p['callsign'] == d['callsign'] and
                              8 <= d['received_at'] - p['received_at'] <= 30]
                if candidates:
                    p = candidates[0]
                    d['ascent_rate'] = (d['altitude'] - p['altitude']) / (d['received_at'] - p['received_at'])
            self.latest[source] = d
            self.history[source].append(d)
            self.json_file.write(json.dumps(d, allow_nan=False) + '\n')
            self.csv.writerow(d)
        self.event('telemetry', d)

    def snapshot(self):
        with self.lock:
            return dict(history={s: list(v) for s, v in self.history.items()},
                        latest=dict(self.latest), radio=dict(self.radio), prediction=self.prediction,
                        predict_busy=self.predict_busy, predict_error=self.predict_error)

    def radio_status(self, **kwargs):
        with self.lock:
            self.radio.update(kwargs)
            data = dict(self.radio)
        self.event('radio', data)

    def start_prediction(self, options):
        source = options.get('source')
        if source not in self.history:
            raise ValueError('Wybierz RS41 albo T-Beam')
        with self.lock:
            if self.predict_busy:
                raise ValueError('Predykcja już trwa')
            d = dict(self.latest.get(source, {}))
            if not position_ok(d):
                raise ValueError('Wybrane źródło nie ma poprawnej pozycji GPS')
            if time.time() - d['received_at'] > 30:
                raise ValueError('Pozycja jest starsza niż 30 s; poczekaj na świeżą telemetrię')
            params = build_prediction(d, options)
            self.predict_busy = True
            self.predict_error = ''
        self.event('prediction_status', {'busy': True, 'error': ''})
        threading.Thread(target=self._predict, args=(d, params, options.get('phase')), daemon=True).start()

    def _predict(self, origin, params, phase):
        try:
            req = urllib.request.Request(PREDICTOR + '?' + urllib.parse.urlencode(params),
                                         headers={'User-Agent': 'Hawkeye-local-panel/1.0'})
            with urllib.request.urlopen(req, timeout=25) as r:
                payload = r.read(5_000_001)
                if len(payload) > 5_000_000:
                    raise ValueError('Za duża odpowiedź predyktora')
                result = json.loads(payload)
            if result.get('error'):
                raise ValueError(str(result['error']))
            stages = result.get('prediction', [])
            if phase == 'descent':
                stages = [s for s in stages if s.get('stage') == 'descent']
            points = [p for s in stages for p in s.get('trajectory', [])]
            if not points:
                raise ValueError('Predyktor zwrócił pustą trasę')
            for p in points:
                if any(number(p.get(k)) is None for k in ('latitude', 'longitude', 'altitude')):
                    raise ValueError('Nieprawidłowe punkty predykcji')
                p['longitude'] = (float(p['longitude']) + 180) % 360 - 180
            prediction = dict(created_at=time.time(), source=origin['source'], origin=origin,
                              phase=phase, request=result.get('request', params), stages=stages,
                              landing=points[-1])
            with self.lock:
                self.prediction = prediction
                with open(self.json_path.replace('.jsonl', '-predictions.jsonl'), 'a', encoding='utf-8') as f:
                    f.write(json.dumps(prediction, allow_nan=False) + '\n')
            self.event('prediction', prediction)
        except Exception as e:
            with self.lock:
                self.predict_error = str(e)
        finally:
            with self.lock:
                self.predict_busy = False
                state = {'busy': False, 'error': self.predict_error}
            self.event('prediction_status', state)

    def audio_loop(self, args):
        cmd = [sys.executable, '-u', str(Path(__file__).with_name('rs41_worker.py')),
               '--port', str(args.audio_port), '--sample-rate', str(args.sample_rate),
               '--baud', str(args.horus_baud), '--spacing', str(args.tone_spacing)]
        while not self.stop.is_set():
            try:
                self.radio_status(detail='Uruchamianie dekodera', audio=None)
                self.worker = subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True)
                for line in self.worker.stdout:
                    try:
                        msg = json.loads(line)
                        kind, data = msg['kind'], msg['data']
                        if kind == 'packet':
                            self.ingest(horus_packet(data))
                        elif kind == 'audio':
                            self.radio_status(audio=data)
                        elif kind in ('status', 'error'):
                            self.radio_status(detail=data)
                        elif kind == 'warning':
                            self.hub.add_line('[RS41] ' + data)
                    except (ValueError, TypeError, KeyError) as e:
                        self.hub.add_line('[RS41] Błąd ramki: ' + str(e))
                code = self.worker.wait()
                if not self.stop.is_set():
                    self.radio_status(detail=f'Dekoder zatrzymany ({code}). Sprawdź terminal/zależności i zamknij Horus GUI. Ponowienie za 10 s.', audio=None)
            except OSError as e:
                self.radio_status(detail=str(e), audio=None)
            if self.stop.wait(10):
                break

    def summary_loop(self, port):
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind(('', port))
                sock.settimeout(1)
                self.radio_status(detail=f'Tryb Horus GUI: odbiór JSON UDP na {port}')
                while not self.stop.is_set():
                    try:
                        raw = sock.recv(65535)
                        d = json.loads(raw)
                        if d.get('type') == 'PAYLOAD_SUMMARY':
                            self.ingest(horus_packet(d))
                    except socket.timeout:
                        pass
                    except (ValueError, TypeError, AttributeError) as e:
                        self.hub.add_line('[RS41] Pominięto błędny UDP: ' + str(e))
        except OSError as e:
            self.radio_status(detail='Błąd UDP: ' + str(e))

    def close(self):
        self.stop.set()
        if self.worker and self.worker.poll() is None:
            self.worker.terminate()
            try:
                self.worker.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.worker.kill()
                self.worker.wait()
        with self.lock:
            self.json_file.close()
            self.csv_file.close()


def build_prediction(d, options):
    def bounded(key, lo, hi):
        value = number(options.get(key))
        if value is None or not lo <= value <= hi:
            raise ValueError(f'{key}: podaj wartość od {lo} do {hi}')
        return value
    phase = options.get('phase')
    if phase not in ('ascent', 'descent'):
        raise ValueError('Wybierz fazę lotu')
    ascent = bounded('ascent_rate', .1, 30)
    descent = bounded('descent_rate', .1, 50)
    burst = bounded('burst_altitude', 1, 60000)
    if phase == 'descent':
        # Tawhiri standard_profile requires burst > launch. Approximate an
        # already descending balloon by a 1 m ascent at 10 m/s (0.1 s).
        burst, ascent = d['altitude'] + 1, 10
    elif burst <= d['altitude']:
        raise ValueError('Wysokość pęknięcia musi być wyższa od aktualnej wysokości')
    return dict(profile='standard_profile', launch_latitude=d['latitude'],
                launch_longitude=d['longitude'] % 360, launch_altitude=d['altitude'],
                launch_datetime=dt.datetime.now(dt.timezone.utc).isoformat(),
                ascent_rate=ascent, burst_altitude=burst, descent_rate=descent)
