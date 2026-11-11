#!/usr/bin/env python3
"""Integrated RS41 + T-Beam panel: telemetry, charts, map, predictions and USB commands.

    python3 scripts/panel.py --port /dev/ttyUSB0        then open http://127.0.0.1:8000

It replaces the serial monitor (only one program can use the port at a time): it reads the ground
station's log lines and types `arm` / `fire` / `disarm` into it, exactly as you would in the
monitor. See PANEL_PL.md and scripts/requirements-panel.txt for setup.

The page listens on 127.0.0.1 by default; --lan enables local-network access.
Every command needs a random token that is generated at
start-up and only given to the page itself, so another website open in your browser cannot press
the buttons.
"""

import argparse
import collections
import datetime
import json
import os
import queue
import re
import secrets
import socket
import struct
import sys
import threading
import time
from pathlib import Path
from flight import Flight, ground_packet
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

try:
    import serial
    from serial.tools import list_ports
except ImportError:
    sys.exit("pyserial is missing: pip install pyserial")

COMMANDS = {"arm", "fire", "disarm", "poke"}
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
KEEP_LINES = 5000


class Hub:
    """Log lines from the board, the connection state, and the clients watching them."""

    def __init__(self, log_path):
        self.lines = collections.deque(maxlen=KEEP_LINES)
        self.clients = []
        self.lock = threading.Lock()
        self.connected = False
        self.detail = "starting"
        self.ser = None
        self.flight = None
        self.log_path = log_path
        self.log_file = open(log_path, "a", buffering=1, encoding="utf-8")
        self.rs41_log_path = log_path.removesuffix('.log') + '-rs41.log'
        self.rs41_log_file = open(self.rs41_log_path, "a", buffering=1, encoding="utf-8")

    def _publish(self, event, data):
        for q in list(self.clients):
            q.put((event, data))

    def add_line(self, text, source="tbeam"):
        now = datetime.datetime.now()
        entry = {"t": now.strftime("%H:%M:%S.") + f"{now.microsecond // 1000:03d}", "ts": now.timestamp(), "line": text, "source": source}
        with self.lock:
            self.lines.append(entry)
            target = self.rs41_log_file if source == 'rs41' else self.log_file
            target.write(f"{entry['t']} {text}\n")
            self._publish("line", entry)
        if self.flight and source == 'tbeam':
            packet = ground_packet(text)
            if packet:
                self.flight.ingest(packet)

    def set_status(self, connected, detail):
        with self.lock:
            self.connected, self.detail = connected, detail
            self._publish("status", self.status())
        self.add_line(f"[panel] {'connected to' if connected else 'disconnected:'} {detail}")

    def status(self):
        return {"connected": self.connected, "detail": self.detail}

    def subscribe(self):
        q = queue.Queue()
        with self.lock:
            q.put(("status", self.status()))
            for entry in self.lines:
                q.put(("line", entry))
            if self.flight:
                q.put(("snapshot", self.flight.snapshot()))
            self.clients.append(q)
        return q

    def unsubscribe(self, q):
        with self.lock:
            if q in self.clients:
                self.clients.remove(q)

    def send(self, command):
        """Types a command into the ground station. Returns an error text, or None."""
        ser = self.ser
        if ser is None:
            return "the ground station is not connected"
        try:
            ser.write(command.encode() + b"\n")
        except (serial.SerialException, OSError) as e:
            return f"write failed: {e}"
        return None


def serial_loop(hub, port, baud):
    while True:
        try:
            ser = serial.Serial()
            ser.port, ser.baudrate, ser.timeout = port, baud, 0.2
            # keep DTR / RTS low: on most boards raising them resets the chip
            ser.dtr = False
            ser.rts = False
            ser.open()
            hub.ser = ser
            hub.set_status(True, f"{port} @ {baud}")
            buf = b""
            while True:
                buf += ser.read(256)
                while b"\n" in buf:
                    raw, buf = buf.split(b"\n", 1)
                    text = ANSI.sub("", raw.decode("utf-8", "replace")).rstrip("\r")
                    if text:
                        hub.add_line(text)
        except (serial.SerialException, OSError) as e:
            hub.ser = None
            hub.set_status(False, f"{port}: {e}; retrying")
            time.sleep(2)


def make_handler(hub, token, host_names, arm_window_s):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def _host_ok(self):
            # Explicit local IP/hostname allowlist also blocks DNS rebinding.
            return self.headers.get("Host", "") in host_names

        def _reply(self, code, body, ctype="text/plain; charset=utf-8"):
            data = body if isinstance(body, bytes) else body.encode()
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            # Rejections may occur before the POST body has been consumed
            # (e.g. a browser tab with a token from before a server restart).
            # Do not reuse that connection with unread JSON as a request line.
            # SSE uses its own response path and remains open.
            self.send_header("Connection", "close")
            self.close_connection = True
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if not self._host_ok():
                return self._reply(403, "bad host")
            if self.path == "/":
                page = PAGE.replace("__TOKEN__", token).replace("__ARM_WINDOW__", str(arm_window_s))
                return self._reply(200, page, "text/html; charset=utf-8")
            assets = {"/dashboard.js": "dashboard.js", "/leaflet.js": "leaflet.js", "/leaflet.css": "leaflet.css"}
            if self.path in assets:
                asset = Path(__file__).parent / "web" / assets[self.path]
                ctype = "text/css" if asset.suffix == ".css" else "application/javascript"
                return self._reply(200, asset.read_bytes(), ctype)
            if self.path in ("/telemetry.csv", "/telemetry.jsonl"):
                path = hub.flight.csv_path if self.path.endswith("csv") else hub.flight.json_path
                with hub.flight.lock:
                    data = Path(path).read_bytes()
                return self._reply(200, data, "text/csv" if path.endswith("csv") else "application/x-ndjson")
            if self.path in ("/log", "/rs41.log"):
                with open(hub.rs41_log_path if self.path == '/rs41.log' else hub.log_path, "rb") as f:
                    return self._reply(200, f.read())
            if self.path == "/events":
                return self._events()
            self._reply(404, "not found")

        def _events(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            q = hub.subscribe()
            try:
                while True:
                    try:
                        event, data = q.get(timeout=15)
                        msg = f"event: {event}\ndata: {json.dumps(data)}\n\n"
                    except queue.Empty:
                        msg = ": keep-alive\n\n"
                    self.wfile.write(msg.encode())
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, OSError):
                pass
            finally:
                hub.unsubscribe(q)

        def do_POST(self):
            if not self._host_ok() or self.path not in ("/cmd", "/predict"):
                return self._reply(403, "forbidden")
            origin = self.headers.get("Origin")
            if origin is not None and origin.removeprefix("http://") not in host_names:
                return self._reply(403, "bad origin")
            if not secrets.compare_digest(self.headers.get("X-Token", ""), token):
                return self._reply(403, "bad token")
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 4096:
                    return self._reply(400, "bad request length")
                payload = json.loads(self.rfile.read(length))
                if not isinstance(payload, dict):
                    raise ValueError("expected object")
                if self.path == "/predict":
                    try:
                        hub.flight.start_prediction(payload)
                    except ValueError as e:
                        return self._reply(400, str(e))
                    return self._reply(202, "ok")
                command = payload["cmd"]
                if not isinstance(command, str):
                    raise ValueError("bad command")
            except (ValueError, KeyError, TypeError):
                return self._reply(400, "bad request")
            if command not in COMMANDS:
                return self._reply(400, "unknown command")
            error = hub.send(command)
            if error:
                return self._reply(503, error)
            self._reply(200, "ok")

    return Handler


def find_port():
    ports = [p.device for p in list_ports.comports() if "ttyUSB" in p.device or "ttyACM" in p.device]
    if len(ports) == 1:
        return ports[0]
    sys.exit("give the serial port of the ground station with --port (found: %s)" % (", ".join(ports) or "none"))


def local_ipv4_addresses():
    """Discover interface addresses without sending data to an internet service."""
    addresses = {'127.0.0.1'}
    try:
        addresses.update(info[4][0] for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET))
    except OSError:
        pass
    try:
        import fcntl
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            for _, interface in socket.if_nameindex():
                try:
                    request = struct.pack('256s', interface.encode()[:15])
                    result = fcntl.ioctl(sock.fileno(), 0x8915, request)  # Linux SIOCGIFADDR
                    addresses.add(socket.inet_ntoa(result[20:24]))
                except OSError:
                    pass
    except (ImportError, OSError):
        pass
    return sorted(a for a in addresses if a != '0.0.0.0')


def http_settings(lan, port):
    addresses = local_ipv4_addresses() if lan else ['127.0.0.1']
    names = {f'{address}:{port}' for address in addresses} | {f'localhost:{port}'}
    if port == 80:
        names.update(addresses)
        names.add('localhost')
    return ('0.0.0.0' if lan else '127.0.0.1'), names, addresses


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--port", help="serial port of the ground station (default: the only USB serial port)")
    ap.add_argument("--baud", type=int, default=115200)
    ap.add_argument("--http-port", type=int, default=8000, help="port of the web page (default 8000)")
    ap.add_argument("--lan", action="store_true", help="share the panel on local network interfaces (Wi-Fi/LAN)")
    ap.add_argument("--arm-window", type=int, default=15, help="seconds FIRE is allowed after ARM (ARM_WINDOW_MS)")
    ap.add_argument("--rs41-mode", choices=["audio", "summary", "off"], default="audio")
    ap.add_argument("--audio-port", type=int, default=7355)
    ap.add_argument("--sample-rate", type=int, default=48000)
    ap.add_argument("--horus-baud", type=int, default=100)
    ap.add_argument("--tone-spacing", type=int, default=270, help="-1 disables mask estimator")
    ap.add_argument("--summary-port", type=int, default=55672)
    ap.add_argument("--callsign", default="HAWK", help="RS41 callsign filter; empty string accepts any")
    ap.add_argument("--no-serial", action="store_true", help="RS41 only; commands disabled")
    ap.add_argument("--demo", action="store_true", help="synthetic data, no USB, no commands, no decoder")
    args = ap.parse_args()
    port = None if args.no_serial or args.demo else (args.port or find_port())

    os.makedirs("logs", exist_ok=True)
    log_path = f"logs/panel-{datetime.datetime.now():%Y%m%d-%H%M%S}.log"
    hub = Hub(log_path)
    hub.flight = Flight(hub, log_path.removesuffix(".log"), args.callsign or None)
    token = secrets.token_urlsafe(24)
    bind_host, host_names, addresses = http_settings(args.lan, args.http_port)

    if port:
        threading.Thread(target=serial_loop, args=(hub, port, args.baud), daemon=True).start()
    else:
        hub.set_status(False, "DEMO — komendy wyłączone" if args.demo else "USB wyłączone (--no-serial)")
    if args.demo:
        from demo_flight import run_demo
        threading.Thread(target=run_demo, args=(hub.flight,), daemon=True).start()
    elif args.rs41_mode == "audio":
        threading.Thread(target=hub.flight.audio_loop, args=(args,), daemon=True).start()
    elif args.rs41_mode == "summary":
        threading.Thread(target=hub.flight.summary_loop, args=(args.summary_port,), daemon=True).start()
    else:
        hub.flight.radio_status(detail="RS41 wyłączone")
    server = ThreadingHTTPServer((bind_host, args.http_port), make_handler(hub, token, host_names, args.arm_window))
    server.daemon_threads = True
    print(f"panel: http://127.0.0.1:{args.http_port}   log: {log_path}   (Ctrl+C to stop)")
    if args.lan:
        for address in addresses:
            if not address.startswith('127.'):
                print(f'Wi-Fi/LAN: http://{address}:{args.http_port}', flush=True)
        print('LAN: urządzenia mające dostęp do strony mogą także używać komend. Używaj zaufanej sieci.', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        hub.flight.close()
        server.server_close()


PAGE = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Hawkeye · wspólna stacja naziemna</title>
<link rel="stylesheet" href="/leaflet.css">
<script src="/leaflet.js"></script>
<style>
  :root {
    --bg: #f4f5f7; --card: #fff; --text: #1c2026; --muted: #667085; --line: #e1e4ea;
    --ok: #138a4b; --warn: #b26a00; --bad: #c62828; --accent: #2457d6;
    --logbg: #11151b; --logtext: #d6dde8;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #12151a; --card: #1b2028; --text: #e6e9ee; --muted: #8b95a5; --line: #2b323d;
      --ok: #3ccf82; --warn: #f0a63a; --bad: #ff6b6b; --accent: #7ba3ff;
      --logbg: #0b0e12; --logtext: #d6dde8;
    }
  }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text); font: 15px/1.4 system-ui, sans-serif; }
  /* the page is as tall as the screen; the log card takes the space that is left */
  main { max-width: 1100px; margin: 0 auto; padding: 16px; display: flex; flex-direction: column; gap: 16px;
         min-height: 100vh; }
  main > * { flex-shrink: 0; }
  header { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; justify-content: space-between; }
  h1 { font-size: 20px; margin: 0; }
  .pill { padding: 3px 10px; border-radius: 999px; font-size: 13px; border: 1px solid var(--line); background: var(--card); }
  .pill.ok { color: var(--ok); } .pill.bad { color: var(--bad); }
  .card { background: var(--card); border: 1px solid var(--line); border-radius: 10px; padding: 14px; }
  .banner { display: none; padding: 12px 14px; border-radius: 10px; font-weight: 600; color: #fff; background: var(--bad); }
  .controls { display: flex; flex-wrap: wrap; gap: 12px; align-items: center; }
  button { font: inherit; font-weight: 600; padding: 12px 22px; border-radius: 8px; border: 1px solid var(--line);
           background: var(--card); color: var(--text); cursor: pointer; }
  button:disabled { opacity: .4; cursor: not-allowed; }
  button.arm { background: var(--warn); border-color: var(--warn); color: #fff; }
  button.fire { background: var(--bad); border-color: var(--bad); color: #fff; }
  button.disarm { background: var(--ok); border-color: var(--ok); color: #fff; }
  button.small { padding: 5px 12px; font-weight: 500; font-size: 13px; }
  .state { font-size: 22px; font-weight: 700; padding: 4px 14px; border-radius: 8px; border: 2px solid var(--line); }
  .state.Safe { color: var(--ok); border-color: var(--ok); }
  .state.Armed { color: var(--warn); border-color: var(--warn); }
  .state.Firing { color: var(--bad); border-color: var(--bad); }
  .hint { color: var(--muted); font-size: 13px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 10px; }
  .stat .k { color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: .04em; }
  .stat .v { font-size: 18px; font-weight: 600; font-variant-numeric: tabular-nums; word-break: break-word; }
  .stat .v.bad { color: var(--bad); } .stat .v.ok { color: var(--ok); }
  .logtools { display: flex; flex-wrap: wrap; gap: 10px; align-items: center; margin-bottom: 8px; }
  .logtools .spacer { flex: 1; }
  .logcard { height: 380px; flex: 1 1 0; display: flex; flex-direction: column; min-height: 260px; }
  #log { background: var(--logbg); color: var(--logtext); border-radius: 8px; padding: 10px; flex: 1; min-height: 0; overflow: auto;
         font: 12.5px/1.45 ui-monospace, Menlo, Consolas, monospace; white-space: pre-wrap; word-break: break-all; }
  /* one row per log line: time | type tag | message. The type sets the colour. */
  #log > div { display: grid; grid-template-columns: 12ch 6ch 1fr; column-gap: 10px; }
  #log .t, #log .tag { white-space: nowrap; word-break: normal; }
  #log .t { color: #6c7686; }
  #log .tag { font-weight: 700; letter-spacing: .03em; }
  #log .TEL { color: #7fb2ff; } #log .STATE { color: #5fd9c0; } #log .CMD { color: #f2c35b; }
  #log .POKE { color: #c79bff; } #log .SYS { color: #8d97a6; } #log .WARN { color: #f0a050; }
  #log .ERR { color: #ff7b7b; } #log .ALERT { color: #ff7b7b; font-weight: 700; }
  #log .ALERT .msg { font-weight: 700; }
  #log .SYS .msg { color: #8d97a6; }
  dialog { border: 1px solid var(--line); border-radius: 12px; background: var(--card); color: var(--text); padding: 20px; max-width: 380px; }
  dialog::backdrop { background: rgba(0,0,0,.55); }
  dialog .row { display: flex; gap: 10px; justify-content: flex-end; margin-top: 16px; }
  .flightgrid { display:grid; grid-template-columns:repeat(auto-fit,minmax(280px,1fr)); gap:16px; }
  h2 {font-size:17px;margin:0 0 12px} .sourcehead {display:flex;justify-content:space-between;gap:10px;align-items:center}
  .sourcehead span {font-size:12px;color:var(--muted)}
  .readings {display:grid;grid-template-columns:repeat(2,1fr);gap:10px}
  .readings small {display:block;color:var(--muted)} .readings b {font-variant-numeric:tabular-nums}
  .plot {width:100%;height:190px;display:block} .chartcard h2 {font-size:14px;margin-bottom:3px}
  #flightmap {height:400px;background:#202b37;border-radius:8px} .legend {display:flex;flex-wrap:wrap;gap:18px;margin:8px 0;font-size:13px}
  .legend .rs {color:#69a7ff}.legend .tb {color:#13bba4}.legend .pr {color:#e7a436}
  .predictionform {display:flex;flex-wrap:wrap;gap:12px;align-items:end}
  .predictionform label {font-size:12px;color:var(--muted);display:flex;flex-direction:column;gap:5px}
  input,select {font:inherit;color:var(--text);background:var(--card);border:1px solid var(--line);border-radius:5px;padding:7px;max-width:170px}
  input[type=checkbox] {width:auto} #prediction-message {white-space:pre-wrap}
  .leaflet-container {color:#1c2026} .leaflet-popup-content {color:#1c2026}
  .logcard #log {flex-basis:260px}
  [hidden] {display:none !important}
  .tabs {display:flex;gap:8px;flex-wrap:wrap;border-bottom:1px solid var(--line);padding-bottom:12px}
  .tabs [aria-selected=true] {background:var(--accent);color:var(--bg);border-color:var(--accent)}
  .comparison-options {display:flex;gap:18px;flex-wrap:wrap;align-items:center}
  .rawlog {background:var(--logbg);color:var(--logtext);border-radius:8px;padding:10px;flex:1;min-height:0;overflow:auto;font:12.5px/1.45 ui-monospace,monospace;white-space:pre-wrap;overflow-wrap:anywhere}
  .rawlog .t {color:#8d97a6}
</style>
</head>
<body>
<main>
  <header>
    <h1>Hawkeye · stacja naziemna</h1>
    <span id="conn" class="pill bad">connecting...</span>
  </header>

  <nav class="tabs" role="tablist" aria-label="Źródło telemetrii">
    <button role="tab" id="tab-tbeam" data-tab="tbeam" aria-selected="true">T-Beam</button>
    <button role="tab" id="tab-rs41" data-tab="rs41" aria-selected="false">RS41</button>
    <button role="tab" id="tab-compare" data-tab="compare" aria-selected="false">Porównanie / nakładanie</button>
  </nav>
  <div class="comparison-options card" id="comparison-options" hidden>
    <strong>Nałóż na wykresy i mapę:</strong>
    <label><input id="show-tbeam" type="checkbox" checked> T-Beam</label>
    <label><input id="show-rs41" type="checkbox" checked> RS41</label>
  </div>
  <div id="alarm" class="banner"></div>

  <section class="card" data-view="tbeam" id="command-card">
    <h2>Komendy T-Beam</h2>
    <div class="controls">
      <span id="state" class="state">?</span>
      <button class="arm" id="arm">ARM</button>
      <button class="fire" id="fire" disabled>FIRE</button>
      <button class="disarm" id="disarm">DISARM</button>
      <button id="poke" title="Manual ping: the balloon answers with how well it heard us">POKE</button>
      <span id="cmdstatus" class="hint"></span>
    </div>
    <p class="hint" id="hint" style="margin-bottom:0">ARM asks the balloon for a one-time code. When it reports READY, FIRE is enabled until the balloon's arm window (<span id="win"></span> s) ends and it drops back to SAFE by itself. The state is updated by each answer to a command or POKE and by the balloon's own reports (periodically and on each change); POKE also tells how well the balloon heard us.</p>
  </section>

  <section class="card" hidden>
    <div class="grid">
      <div class="stat"><div class="k">Last telemetry</div><div class="v" id="age">-</div></div>
      <div class="stat"><div class="k">State as of</div><div class="v" id="sage">-</div></div>
      <div class="stat"><div class="k">GPS fix</div><div class="v" id="fix">-</div></div>
      <div class="stat"><div class="k">Altitude</div><div class="v" id="alt">-</div></div>
      <div class="stat"><div class="k">Position</div><div class="v" id="pos">-</div></div>
      <div class="stat"><div class="k">Pressure</div><div class="v" id="press">-</div></div>
      <div class="stat"><div class="k">Temperature</div><div class="v" id="temp">-</div></div>
      <div class="stat"><div class="k">Humidity</div><div class="v" id="hum">-</div></div>
      <div class="stat"><div class="k">Burns</div><div class="v" id="burns">-</div></div>
      <div class="stat"><div class="k">Radio</div><div class="v" id="rssi">-</div></div>
    </div>
  </section>

  <div class="flightgrid">
    <section class="card" data-source="rs41"><div class="sourcehead"><h2>RS41 <small id="rs-callsign"></small></h2><span id="rs-age">Brak danych</span></div><div id="rs-readings" class="readings"></div><p id="rs-radio" class="hint"></p></section>
    <section class="card" data-source="tbeam"><div class="sourcehead"><h2>T-Beam</h2><span id="tb-age">Brak danych</span></div><div id="tb-readings" class="readings"></div><p class="hint">Telemetria i komendy przez stację USB. Przyciski ARM / FIRE / DISARM / POKE pozostają powyżej.</p></section>
  </div>
  <section class="card">
    <div class="controls"><h2 style="margin:0">Lot na żywo</h2><a href="/telemetry.csv" download>Pobierz CSV</a><a href="/telemetry.jsonl" download>Pobierz JSONL</a><label class="hint"><input type="checkbox" id="chart-hour" checked> Ostatnie 60 minut</label></div>
    <div class="legend"><span class="rs" data-source="rs41">● RS41</span><span class="tb" data-source="tbeam">● T-Beam</span></div>
    <div class="flightgrid" id="charts"></div>
  </section>
  <section class="card">
    <div class="controls"><h2 style="margin:0">Mapa i predykcja</h2><button class="small" id="fit-map">Pokaż cały lot</button><label class="hint"><input type="checkbox" id="follow-map" checked> Śledź pozycję</label><label class="hint"><input type="checkbox" id="map-online"> Włącz podkład mapy (internet)</label></div>
    <div class="legend"><span class="rs" data-source="rs41">● RS41</span><span class="tb" data-source="tbeam">● T-Beam</span><span class="pr">┄ Predykcja</span></div>
    <div id="flightmap"></div>
    <p class="hint">Ślady działają offline. Podkład OpenStreetMap wymaga internetu. Predykcja wysyła wybraną pozycję i parametry lotu do SondeHub/Tawhiri.</p>
    <form id="prediction-form" class="predictionform">
      <label>Pozycja<select id="pred-source"><option value="rs41">RS41</option><option value="tbeam">T-Beam</option></select></label>
      <label>Faza lotu<select id="pred-phase"><option value="ascent">Wznoszenie</option><option value="descent">Opadanie</option></select></label>
      <label>Pęknięcie [m AMSL]<input id="pred-burst" type="number" value="30000" min="1" max="60000" required></label>
      <label>Wznoszenie [m/s]<input id="pred-ascent" type="number" value="5" min="0.1" max="30" step="0.1" required></label>
      <label>Opadanie przy poziomie morza [m/s]<input id="pred-descent" type="number" value="5" min="0.1" max="50" step="0.1" required></label>
      <button id="predict" type="submit">Oblicz predykcję</button>
      <label><span>Odświeżanie</span><span><input id="pred-auto" type="checkbox"> Co 2 minuty</span></label>
    </form>
    <p class="hint">Wartości początkowe są przykładowe — ustaw parametry swojego balonu i spadochronu. Opadanie modelujemy jako pęknięcie 1 m nad bieżącą pozycją; nie wpisuj tutaj aktualnej prędkości opadania na dużej wysokości.</p>
    <p id="prediction-message" class="hint">Predykcja nie została jeszcze obliczona.</p>
    <p id="prediction-result"></p>
  </section>
  <section class="card logcard" data-view="tbeam" id="tbeam-log-card">
    <div class="logtools">
      <strong>Surowy log T-Beam · USB</strong>
      <label><input type="checkbox" id="hideTel"> hide telemetry lines</label>
      <label><input type="checkbox" id="hideState"> hide state lines</label>
      <label><input type="checkbox" id="follow" checked> follow</label>
      <span class="spacer"></span>
      <button class="small" id="clear">Clear view</button>
      <a href="/log" download="panel.log"><button class="small">Download log</button></a>
    </div>
    <div id="log"></div>
  </section>
  <section class="card logcard" data-view="rs41" id="rs41-log-card" hidden>
    <div class="logtools">
      <strong>Surowy log RS41 · HEX / JSON</strong>
      <label><input type="checkbox" id="rs-follow" checked> Przewijaj</label>
      <span class="spacer"></span>
      <button class="small" id="rs-clear">Wyczyść widok</button>
      <a href="/rs41.log" download="rs41.log">Pobierz log RS41</a>
    </div>
    <div id="rs-log" class="rawlog" role="log" aria-label="Surowe ramki i log dekodera RS41"></div>
    <p class="hint">HEX to odebrane ramki radiowe, JSON to pełne pola zwrócone przez dekoder. W trybie Horus GUI dostępny jest tylko JSON UDP. Demo pokazuje oznaczone dane syntetyczne.</p>
  </section>
  <p class="hint" id="compare-log-hint" hidden>Pełne surowe logi znajdziesz w zakładkach T-Beam i RS41. Oba odbiorniki i zapis działają również w tle.</p>
</main>

<dialog id="confirm">
  <h2 style="margin-top:0">Fire the burn output?</h2>
  <p>The balloon will start burning the cutdown wire.</p>
  <div class="row">
    <button id="no">Cancel</button>
    <button class="fire" id="yes">FIRE</button>
  </div>
</dialog>

<script>
const TOKEN = "__TOKEN__", ARM_WINDOW_S = __ARM_WINDOW__;
const $ = id => document.getElementById(id);
const RE_TEL = /telemetry #(\d+) fix (true|false) (-?[\d.]+) (-?[\d.]+) alt (-?[\d.]+) m \| (-?[\d.]+) hPa (-?[\d.]+) C (-?[\d.]+) %RH \| RSSI (-?\d+) dBm, SNR (-?\d+) dB/;
const RE_STATE = /\bstate (\w+) code ([0-9A-F]+) burns (\d+) failsafe (\w+(?:\(\w+\))?)/;
const RE_FAILSAFE = /FAILSAFE CUTDOWN TRIGGERED by the balloon: (\w+)/;

let lastState = null, lastStateAt = 0, armedUntil = 0, lastTelAt = 0, connected = false;
$("win").textContent = ARM_WINDOW_S;

function setText(id, text, cls) { const e = $(id); e.textContent = text; e.className = "v" + (cls ? " " + cls : ""); }

function onLine(line, ts) {
  let m;
  if ((m = RE_TEL.exec(line))) {
    lastTelAt = ts;
    setText("fix", m[2] === "true" ? "yes" : "NO FIX", m[2] === "true" ? "ok" : "bad");
    setText("alt", m[5] + " m");
    setText("pos", m[3] + ", " + m[4]);
    setText("press", m[6] + " hPa");
    setText("temp", m[7] + " °C");
    setText("hum", m[8] + " %");
    setText("rssi", m[9] + " dBm / SNR " + m[10] + " dB");
  } else if ((m = RE_STATE.exec(line))) {
    // the state arrives as an answer to a command or a poke, and periodically; Armed ends by itself
    // the arm window runs from the first ARMED report (a new code starts a new one)
    const sameArm = lastState && lastState.state === "Armed" && lastState.code === m[2];
    lastState = { state: m[1], code: m[2], burns: +m[3] };
    lastStateAt = ts;
    if (m[1] !== "Armed") armedUntil = 0;
    else if (!sameArm || !armedUntil) armedUntil = ts + ARM_WINDOW_S * 1000;
    setText("burns", m[3]);
  } else if ((m = RE_FAILSAFE.exec(line))) {
    const b = $("alarm"); b.style.display = "block";
    b.textContent = "FAILSAFE CUTDOWN TRIGGERED by the balloon (" + m[1] + ")";
  }
  if (line.includes("*** READY")) $("cmdstatus").textContent = "READY: balloon is armed";
  else if (line.includes("CONFIRMED")) $("cmdstatus").textContent = line.replace(/^.*\*\*\* /, "");
  else if (line.includes("*** POKE reply")) $("cmdstatus").textContent = line.replace(/^.*\*\*\* /, "");
  else if (line.includes("no reply to poke")) $("cmdstatus").textContent = "no reply to the poke";
  else if (line.includes("no confirmation")) $("cmdstatus").textContent = "no confirmation, try POKE to see the state";
  else if (line.includes(">>> NOT sending FIRE")) $("cmdstatus").textContent = "FIRE not sent: balloon is not armed";
}

function refresh() {
  const age = lastTelAt ? (Date.now() - lastTelAt) / 1000 : null;
  const stale = age === null || age > 15;
  setText("age", age === null ? "none yet" : age.toFixed(0) + " s ago", stale ? "bad" : "ok");
  const sAge = lastState ? (Date.now() - lastStateAt) / 1000 : null;
  setText("sage", sAge === null ? "never (POKE)" : sAge.toFixed(0) + " s ago");
  const s = $("state");
  // Armed times out by itself after the arm window, here as on the balloon
  const left = armedUntil ? Math.max(0, (armedUntil - Date.now()) / 1000) : 0;
  const shown = lastState ? (lastState.state === "Armed" && left <= 0 ? "Safe" : lastState.state) : null;
  if (shown) { s.textContent = shown.toUpperCase(); s.className = "state " + shown; }
  else { s.textContent = "?"; s.className = "state"; }
  const armed = shown === "Armed";
  $("fire").disabled = !(connected && armed && left > 0);
  $("fire").textContent = armed && left > 0 ? "FIRE (" + left.toFixed(0) + " s)" : "FIRE";
  $("arm").disabled = !connected;
  $("disarm").disabled = !connected;
  $("poke").disabled = !connected;
}
setInterval(refresh, 250);

async function send(cmd) {
  $("cmdstatus").textContent = "sending " + cmd + "...";
  try {
    const r = await fetch("/cmd", { method: "POST", headers: { "X-Token": TOKEN, "Content-Type": "application/json" },
                                    body: JSON.stringify({ cmd }) });
    $("cmdstatus").textContent = r.ok ? cmd + " sent, waiting for the balloon" : "failed: " + (await r.text());
  } catch (e) { $("cmdstatus").textContent = "failed: " + e; }
}
$("arm").onclick = () => send("arm");
$("disarm").onclick = () => send("disarm");
$("poke").onclick = () => send("poke");
$("fire").onclick = () => $("confirm").showModal();
$("no").onclick = () => $("confirm").close();
$("yes").onclick = () => { $("confirm").close(); send("fire"); };

const log = $("log");
// Every line is shown as: time | TYPE | message, with the type deciding the colour. The "INFO - " /
// "WARN - " prefix of the board is dropped (the type column says it) and "[panel] " likewise.
const TAGS = { TEL: "TEL", STATE: "STATE", CMD: "CMD", POKE: "POKE", SYS: "SYS", WARN: "WARN", ERR: "ERR", ALERT: "ALERT" };
function classify(raw) {
  let level = "INFO", text = raw, m;
  if ((m = /^(INFO|WARN|ERROR) - (.*)$/.exec(raw))) { level = m[1]; text = m[2]; }
  else if (raw.startsWith("[panel] ")) { text = raw.slice(8); level = "PANEL"; }
  else level = "RAW"; // boot messages, anything not from the firmware's logger
  let kind;
  if (text.includes("!!!") || /FAILSAFE/.test(text)) kind = "ALERT";
  else if (/^telemetry #\d+/.test(text)) kind = "TEL";
  else if (/^state \w+ code /.test(text)) kind = "STATE";
  else if (/(^|>>> )poke #|POKE reply|no reply to poke|pong/.test(text)) kind = "POKE";
  else if (/^>>> |\*\*\* |^command: /.test(text)) kind = "CMD";
  else if (level === "ERROR") kind = "ERR";
  else if (level === "WARN") kind = "WARN";
  else kind = "SYS";
  return { kind, text: text.replace(/^\*\*\* |^>>> /, "") };
}
function addLine(entry) {
  const { kind, text } = classify(entry.line);
  const div = document.createElement("div");
  div.className = kind;
  div.dataset.kind = kind;
  div.hidden = isHidden(kind);
  const t = document.createElement("span"); t.className = "t"; t.textContent = entry.t;
  const tag = document.createElement("span"); tag.className = "tag"; tag.textContent = TAGS[kind];
  const msg = document.createElement("span"); msg.className = "msg"; msg.textContent = text;
  div.append(t, tag, msg);
  log.append(div);
  while (log.childElementCount > 3000) log.firstChild.remove();
  if ($("follow").checked) log.scrollTop = log.scrollHeight;
}
const isHidden = kind => (kind === "TEL" && $("hideTel").checked) || (kind === "STATE" && $("hideState").checked);
const applyFilters = () => { for (const d of log.children) d.hidden = isHidden(d.dataset.kind); };
$("hideTel").onchange = applyFilters;
$("hideState").onchange = applyFilters;
$("clear").onclick = () => { log.textContent = ""; };
log.addEventListener("scroll", () => { if (log.scrollTop + log.clientHeight < log.scrollHeight - 8) $("follow").checked = false; });

// Register both dashboard and log listeners before opening the SSE stream.
const es = new EventTarget();
es.addEventListener("line", e => { const entry = JSON.parse(e.data); if (entry.source !== "rs41") { addLine(entry); onLine(entry.line, entry.ts * 1000); } });
es.addEventListener("status", e => {
  const s = JSON.parse(e.data);
  connected = s.connected;
  $("conn").textContent = s.connected ? "ground station: " + s.detail : "ground station not connected";
  $("conn").className = "pill " + (s.connected ? "ok" : "bad");
});
es.onerror = () => { $("conn").textContent = "panel server not reachable"; $("conn").className = "pill bad"; connected = false; };
</script>
<script src="/dashboard.js"></script>
</body>
</html>
"""

if __name__ == "__main__":
    main()
