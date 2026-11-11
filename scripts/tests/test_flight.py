import csv
import io
import json
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
import urllib.error
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from flight import Flight, ground_packet, horus_packet, position_ok, build_prediction
from panel import Hub, ThreadingHTTPServer, make_handler, http_settings

LINE = 'INFO - telemetry #24 fix true 52.17818 21.18629 alt 106.0 m | 995.4 hPa 21.5 C 46.0 %RH | RSSI -91 dBm, SNR 10 dB'
OPTIONS = dict(source='rs41', phase='ascent', burst_altitude=30000, ascent_rate=5, descent_rate=5)


class FlightTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.hub = Hub(str(Path(self.tmp.name)/'panel.log'))
        self.flight = Flight(self.hub, str(Path(self.tmp.name)/'panel'), 'HAWK')
        self.hub.flight = self.flight

    def tearDown(self):
        self.flight.close()
        self.hub.log_file.close()
        self.hub.rs41_log_file.close()
        self.tmp.cleanup()

    def packet(self, **extra):
        return horus_packet(dict(callsign='HAWK', sequence_number=24, latitude=52.17818,
            longitude=21.18629, altitude=106, satellites=8, ext_temperature=21.5,
            temperature=27, temperature_custom_1=26.4, ext_pressure=995.4,
            battery_voltage=3.9, ext_humidity=46, **extra))

    def test_ground_parser_and_hub_integration(self):
        self.hub.add_line(LINE)
        p = self.flight.latest['tbeam']
        self.assertTrue(p['fix'])
        self.assertEqual((p['temperature'], p['rssi']), (21.5, -91))
        self.assertIsNone(ground_packet('INFO - state Safe'))

    def test_no_fix_invalid_coordinates_and_zero_satellites(self):
        d = self.packet(); d['satellites'] = 0
        # Feed satellite count into the normalizer, as received from decoder.
        self.assertFalse(horus_packet(dict(callsign='HAWK',latitude=52,longitude=21,
                                          altitude=100,satellites=0))['fix'])
        d['latitude'] = 999
        self.assertFalse(position_ok(d))
        self.flight.ingest(d)
        self.assertFalse(self.flight.latest['rs41']['fix'])
        with self.assertRaises(ValueError):
            self.flight.start_prediction(OPTIONS)

    def test_callsign_filter_duplicates_and_exports(self):
        d = self.packet()
        self.flight.ingest(dict(d,callsign='OTHER'))
        self.assertNotIn('rs41', self.flight.latest)
        self.flight.ingest(d); self.flight.ingest(d)
        self.assertEqual(len(self.flight.history['rs41']),1)
        with open(self.flight.csv_path) as f:
            rows=list(csv.DictReader(f))
        self.assertEqual(len(rows),1)
        self.assertEqual(rows[0]['temperature'],'21.5')
        snapshot=self.flight.snapshot()
        self.assertEqual(snapshot['latest']['rs41']['internal_temperature'],27)
        self.assertEqual(snapshot['latest']['rs41']['custom_temperature'],26.4)

    def test_true_horus_v3_frame_units(self):
        from horusdemodlib.decoder import HORUS_ASN, decode_packet
        from horusdemodlib.checksums import add_packet_crc
        data=dict(payloadCallsign='HAWK',sequenceNumber=24,timeOfDaySeconds=43275,
            latitude=5217818,longitude=2118629,altitudeMeters=106,
            gnssSatellitesVisible=8,pressurehPa_x10=9954)
        data.pop('pressurehPa_x10')
        data['pressurehPa-x10']=9954
        data['temperatureCelsius-x10']={'internal':270,'external':215,'custom1':264}
        data['humidityPercentage']=46
        data['milliVolts']={'battery':3900}
        frame=add_packet_crc(HORUS_ASN.encode('Telemetry',data),tail=False)
        normalized=horus_packet(decode_packet(frame))
        self.assertEqual(normalized['temperature'],21.5)
        self.assertEqual(normalized['pressure'],995.4)
        self.assertEqual(normalized['battery'],3.9)
        self.assertTrue(position_ok(normalized))
        corrupt=bytearray(frame);corrupt[5]^=1
        with self.assertRaises(Exception):
            decode_packet(bytes(corrupt))

    def test_summary_aliases(self):
        d=horus_packet(dict(type='PAYLOAD_SUMMARY',callsign='HAWK',latitude=52,
                           longitude=21,altitude=100,sats=0,batt_voltage=3.7))
        self.assertFalse(d['fix']);self.assertEqual(d['battery'],3.7)

    def test_lan_is_opt_in_and_uses_explicit_local_addresses(self):
        with patch('panel.local_ipv4_addresses',return_value=['127.0.0.1','192.168.1.20']):
            bind,names,_=http_settings(False,8000)
            self.assertEqual(bind,'127.0.0.1')
            self.assertNotIn('192.168.1.20:8000',names)
            bind,names,_=http_settings(True,8000)
            self.assertEqual(bind,'0.0.0.0')
            self.assertIn('192.168.1.20:8000',names)
            self.assertNotIn('attacker.example:8000',names)
            self.assertIn('192.168.1.20',http_settings(True,80)[1])

    def test_lan_host_and_origin_validation(self):
        server=ThreadingHTTPServer(('127.0.0.1',0),make_handler(self.hub,'token',set(),15))
        port=server.server_address[1]
        authority=f'192.168.1.20:{port}'
        server.RequestHandlerClass=make_handler(self.hub,'token',{authority},15)
        server.daemon_threads=True
        t=threading.Thread(target=server.serve_forever,daemon=True);t.start()
        base=f'http://127.0.0.1:{port}'
        try:
            with urllib.request.urlopen(urllib.request.Request(base,headers={'Host':authority}),timeout=3) as r:
                self.assertEqual(r.status,200)
            # Same-origin LAN prediction reaches validation rather than a 403.
            for origin,token,expected in [('http://'+authority,'token',400),
                                          ('http://attacker.example','token',403),
                                          ('http://'+authority,'wrong',403)]:
                req=urllib.request.Request(base+'/predict',data=b'{"source":"invalid"}',
                    headers={'Host':authority,'Origin':origin,'X-Token':token})
                with self.assertRaises(urllib.error.HTTPError) as e:
                    urllib.request.urlopen(req,timeout=3)
                self.assertEqual(e.exception.code,expected)
            with self.assertRaises(urllib.error.HTTPError) as e:
                urllib.request.urlopen(urllib.request.Request(base,headers={'Host':'attacker.example'}),timeout=3)
            self.assertEqual(e.exception.code,403)
        finally:
            server.shutdown();server.server_close();t.join()

    def test_raw_logs_are_separate_and_include_duplicate_frames(self):
        packet=dict(callsign='HAWK',sequence_number=12,latitude=52,longitude=21,
                    altitude=106,satellites=8,raw_hex='0123AABB',ext_temperature=21.5)
        self.flight.receive_horus(packet)
        self.flight.receive_horus(packet)
        self.hub.add_line(LINE)
        rs41=Path(self.hub.rs41_log_path).read_text()
        ground=Path(self.hub.log_path).read_text()
        self.assertEqual(rs41.count('0123AABB'),2)
        self.assertNotIn('0123AABB',ground)
        self.assertIn('telemetry #24',ground)
        self.assertNotIn('telemetry #24',rs41)
        self.assertEqual(len(self.flight.history['rs41']),1)
        self.assertTrue(all(e['source']=='rs41' for e in list(self.hub.lines)[:2]))

    def test_prediction_parameters_stale_and_descent(self):
        self.flight.ingest(self.packet())
        d=self.flight.latest['rs41']
        p=build_prediction(d,dict(OPTIONS,phase='descent'))
        self.assertEqual(p['burst_altitude'],107)
        self.assertEqual(p['ascent_rate'],10)
        with self.assertRaises(ValueError):
            build_prediction(d,dict(OPTIONS,ascent_rate=float('nan')))
        d['received_at']-=40
        with self.assertRaises(ValueError):
            self.flight.start_prediction(OPTIONS)

    def test_prediction_success_and_network_failure_keep_last(self):
        self.flight.ingest(self.packet())
        d=self.flight.latest['rs41']
        payload=dict(prediction=[dict(stage='descent',trajectory=[
            dict(latitude=52,longitude=350,altitude=100,datetime='2026-11-11T13:00:00Z')])])
        with patch('flight.urllib.request.urlopen',return_value=io.BytesIO(json.dumps(payload).encode())):
            self.flight._predict(d,build_prediction(d,OPTIONS),'descent')
        self.assertEqual(self.flight.prediction['landing']['longitude'],-10)
        previous=self.flight.prediction
        with patch('flight.urllib.request.urlopen',side_effect=OSError('offline')):
            self.flight._predict(d,build_prediction(d,OPTIONS),'descent')
        self.assertIs(self.flight.prediction,previous)
        self.assertEqual(self.flight.predict_error,'offline')
        self.assertFalse(self.flight.predict_busy)

    def test_http_commands_assets_and_sse_snapshot(self):
        server=ThreadingHTTPServer(('127.0.0.1',0),make_handler(self.hub,'test-token',
                                   set(),15))
        port=server.server_address[1]
        server.RequestHandlerClass=make_handler(self.hub,'test-token',{f'127.0.0.1:{port}'},15)
        server.daemon_threads=True
        t=threading.Thread(target=server.serve_forever,daemon=True);t.start()
        base=f'http://127.0.0.1:{port}'
        class SerialEmulator:
            def __init__(self):self.writes=[]
            def write(self,b):self.writes.append(b)
        emulator=SerialEmulator();self.hub.ser=emulator
        def request(path,body,token='test-token'):
            return urllib.request.urlopen(urllib.request.Request(base+path,data=json.dumps(body).encode(),
                headers={'Content-Type':'application/json','X-Token':token}),timeout=3)
        try:
            self.hub.add_line(LINE)
            self.hub.add_line('CRC OK HEX AABBCC',source='rs41')
            for path in ('/','/dashboard.js','/leaflet.js','/leaflet.css','/telemetry.csv','/telemetry.jsonl','/rs41.log'):
                with urllib.request.urlopen(base+path,timeout=3) as r:
                    self.assertEqual(r.status,200); self.assertGreater(len(r.read()),0)
            for cmd in ('arm','disarm','poke','fire'):
                with request('/cmd',{'cmd':cmd}) as r:self.assertEqual(r.status,200)
            self.assertEqual(emulator.writes,[b'arm\n',b'disarm\n',b'poke\n',b'fire\n'])
            for data,token,expected in [({'cmd':'arm'},'wrong',403),({'cmd':'unknown'},'test-token',400),({'cmd':[]},'test-token',400)]:
                with self.assertRaises(urllib.error.HTTPError) as error:request('/cmd',data,token)
                self.assertEqual(error.exception.code,expected)
            self.hub.ser=None
            with self.assertRaises(urllib.error.HTTPError) as error:request('/cmd',{'cmd':'poke'})
            self.assertEqual(error.exception.code,503)
            with self.assertRaises(urllib.error.HTTPError) as error:
                request('/predict',dict(OPTIONS,source='invalid'))
            self.assertEqual(error.exception.code,400)
        finally:
            self.hub.ser=None;server.shutdown();server.server_close();t.join()
        q=self.hub.subscribe()
        events=[]
        while not q.empty():events.append(q.get()[0])
        self.assertIn('snapshot',events)
        self.hub.unsubscribe(q)

    def test_native_audio_worker_receives_pcm(self):
        with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as available:
            available.bind(('127.0.0.1',0));port=available.getsockname()[1]
        p=subprocess.Popen([sys.executable,'-u',str(Path(__file__).resolve().parents[1]/'rs41_worker.py'),
                            '--port',str(port)],stdout=subprocess.PIPE,text=True)
        try:
            first=json.loads(p.stdout.readline())
            self.assertEqual(first['kind'],'status')
            with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as sender:
                sender.sendto(b'\0'*8192,('127.0.0.1',port))
            report=json.loads(p.stdout.readline())
            self.assertEqual(report['kind'],'audio')
            self.assertEqual(report['data']['peak_dbfs'],-120)
        finally:
            p.terminate();p.wait(timeout=3);p.stdout.close()

    def test_rejected_post_does_not_parse_unread_json_as_next_request(self):
        server=ThreadingHTTPServer(('127.0.0.1',0),make_handler(self.hub,'fresh-token',set(),15))
        port=server.server_address[1]
        host=f'127.0.0.1:{port}'
        server.RequestHandlerClass=make_handler(self.hub,'fresh-token',{host},15)
        server.daemon_threads=True
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        body=json.dumps(OPTIONS).encode()
        # Reproduce two requests on the same TCP stream. The first is rejected
        # before its body is read. The server must close, never interpret JSON
        # plus the second POST as an HTTP method.
        wire=(f'POST /predict HTTP/1.1\r\nHost: {host}\r\nX-Token: old-token\r\n'
              f'Content-Length: {len(body)}\r\n\r\n').encode()+body
        wire+=(f'POST /predict HTTP/1.1\r\nHost: {host}\r\nX-Token: fresh-token\r\n'
               f'Content-Length: {len(body)}\r\n\r\n').encode()+body
        try:
            with socket.create_connection(('127.0.0.1',port),timeout=3) as sock:
                sock.sendall(wire)
                response=b''
                while True:
                    try:
                        block=sock.recv(8192)
                    except ConnectionResetError:
                        break
                    if not block:break
                    response+=block
            self.assertIn(b'403 Forbidden',response)
            self.assertIn(b'Connection: close',response)
            self.assertNotIn(b'501',response)
            self.assertEqual(response.count(b'HTTP/1.1'),1)
        finally:
            server.shutdown();server.server_close();thread.join()


if __name__=='__main__':
    unittest.main()
