# Sonda HAWK · panel RS41 i T-Beam

Lokalna stacja naziemna: odczyty RS41 i T-Beama, osobne zakładki, porównanie
wykresów i śladów, surowe logi oraz predykcja lądowania z internetu.
Poniżej zachowano dokumentację firmware Hawkeye.

## Pierwsze uruchomienie na Fedorze

Jednorazowo zainstaluj narzędzia i pobierz projekt do nowego folderu:

```bash
sudo dnf install git python3-pip python3-devel gcc make libffi-devel
mkdir -p ~/Projekty
cd ~/Projekty
git clone https://github.com/simba-lew/sonda-hawk.git
cd sonda-hawk
python3 -m venv .venv
.venv/bin/python -m pip install -r scripts/requirements-panel.txt
```

Uruchom demo bez sprzętu:

```bash
./start.sh --demo
```

Otwórz http://127.0.0.1:8000 . Zatrzymaj program przez Ctrl+C.
Z podłączoną stacją naziemną i dźwiękiem z SDRangela:

```bash
./start.sh --port /dev/ttyUSB0
```

Horus GUI powinno być zamknięte, ponieważ panel sam dekoduje dźwięk UDP
na 127.0.0.1:7355. Port USB może mieć inną nazwę.
Pełna instrukcja ustawień i predykcji: [PANEL_PL.md](PANEL_PL.md).

## Skróty w terminalu

Po sklonowaniu do `~/Projekty/sonda-hawk` dopisz aliasy do `~/.bashrc`:

```bash
echo 'alias sonda="\"$HOME/Projekty/sonda-hawk/start.sh\" --port /dev/ttyUSB0"' >> ~/.bashrc
echo 'alias sonda-demo="\"$HOME/Projekty/sonda-hawk/start.sh\" --demo"' >> ~/.bashrc
source ~/.bashrc
```

Nowe definicje zastępują wcześniejsze aliasy o tych samych nazwach.
Skrypt `start.sh` sam wybiera środowisko `.venv` i zapisuje logi w folderze projektu.

## Dostęp przez Wi-Fi / LAN

Telefon i laptop muszą być w tej samej sieci lokalnej. Uruchom panel z opcją:

```bash
sonda --lan
# lub test bez sprzętu:
sonda-demo --lan
```

Bez aliasów: `./start.sh --lan --port /dev/ttyUSB0` lub `./start.sh --demo --lan`.
Terminal wypisze adresy `Wi-Fi/LAN: http://...:8000`. Na telefonie otwórz adres
IP laptopa z sieci Wi-Fi, np. `http://192.168.1.20:8000`, nie `localhost`.
Wszystkie zakładki, logi, wykresy i komendy działają w przeglądarce telefonu.
Opcja udostępnia panel na interfejsach IPv4 laptopa, także Ethernet.
Osoby mające dostęp do strony mogą używać komend; ten tryb jest dla zaufanej sieci.

Jeśli Fedora blokuje połączenie, w drugim terminalu tymczasowo otwórz port
w domyślnej strefie zapory:

```bash
sudo firewall-cmd --add-port=8000/tcp
```

Reguła działa do przeładowania zapory lub restartu. Aby ją usunąć:
`sudo firewall-cmd --remove-port=8000/tcp`. Jeśli Wi-Fi ma przypisaną inną strefę,
sprawdź `firewall-cmd --get-active-zones` i użyj `--zone=NAZWA_STREFY`.
Nie trzeba przekierowywać portów w routerze. Sieć gościnna lub izolacja klientów
w routerze może uniemożliwiać połączenie urządzeń. Po zmianie adresu IP zrestartuj panel.

Bez `--lan` strona pozostaje dostępna tylko na laptopie.

## Pobieranie aktualizacji

Zatrzymaj panel, a potem:

```bash
cd ~/Projekty/sonda-hawk
git pull --ff-only
.venv/bin/python -m pip install -r scripts/requirements-panel.txt
```

Uruchom `sonda` lub `sonda-demo` i odśwież stronę przez Ctrl+Shift+R.
`git pull` pobiera tylko zmiany już zatwierdzone w gałęzi `main`.
Jeśli polecenie zgłosi konflikt lokalnych zmian, zachowaj komunikat i poproś o pomoc.
Nie trzeba usuwać swojego folderu ani logów.

Kod jest wersjonowany w repozytorium. Środowisko `.venv`, logi i prywatny klucz
`src/key.rs` są pomijane przez `.gitignore` i pozostają na Twoim komputerze.

---

# Hawkeye · dokumentacja firmware

Firmware for a high-altitude balloon **cutdown**: a radio-controlled release that burns through a
wire to separate the payload. Written in Rust for the ESP32-S3, in two roles built from
one crate:

- **balloon**: flies on the balloon. Sends its position and sensor readings, answers
  commands and pokes with its state, drives the burn output, and cuts itself down if it loses contact with the ground.
- **ground**: stays on the ground. You type commands into its serial monitor; it sends them to the
  balloon and shows what the balloon reports.

## Hardware

| Role    | Board                                  | Notes |
|---------|----------------------------------------|-------|
| balloon | LilyGO **T-Beam S3 Supreme**           | SX1262 radio, u-blox GPS, AXP2101 power chip, BME280 |
| ground  | Heltec **WiFi LoRa 32 V3 / V3.2**      | SX1262 radio, console input over UART0 |

The role selects the board: there are no separate board features. All pin maps are in
`src/board.rs`, which is the only file to change for another board.

## Environment

- Rust `esp` toolchain (`rust-toolchain.toml` selects it), installed with
  [`espup`](https://github.com/esp-rs/espup). It builds `core` itself (`build-std`).
- Before every build in a new shell: `source ~/export-esp.sh` (the file `espup` generates).
- [`espflash`](https://github.com/esp-rs/espflash) to flash and monitor (`cargo install espflash`).
- Main crates: `esp-hal` 1.2 (unstable features), `esp-rtos` + `embassy-executor` / `embassy-time`
  (async tasks), `lora-phy` (radio), `hmac` + `sha2` (signatures), `log` + `esp-println` (logging).
- `DEFMT_LOG=off` is set in `.cargo/config.toml` because `lora-phy` uses defmt, which this project
  does not use.

## Setup

1. **Create the secret key** (same file on both boards) and put the output into `src/key.rs`:

   ```sh
   python3 -c 'import os;print(", ".join("0x%02X" % b for b in os.urandom(32)))'
   ```

Use a new key before every flight.
2. **Build and flash** each board with its own role. Plug in one board at a time or pass the port:

   ```sh
   source ~/export-esp.sh
   cargo run --release --features balloon -- --port /dev/ttyACM0
   cargo run --release --features ground  -- --port /dev/ttyUSB0
   ```

## Using it

The ground station's monitor shows the log. Type a command and press Enter:

| Command | What it does |
|---------|--------------|
| `arm`   | Asks the balloon to arm. It answers with its state, including a fresh one-time code. The ground station prints `*** READY` when it arrives. |
| `fire`  | Sends the code. Allowed only within `ARM_WINDOW_MS` (15 s) of READY. The burn output is active for `BURN_MS`. |
| `disarm`| Back to SAFE; also aborts a running burn. |
| `poke`  | A manual ping that the balloon answers at once. The ground station prints `*** POKE reply` with the round-trip time and how well each side heard the other (RSSI / SNR), or `no reply to poke` after `GROUND_MAX_TRIES` tries. A poke counts as contact for the failsafe. |

A command goes out at once and is repeated every `GROUND_COMMAND_RETRY_MS` (2 s) until the balloon's
state reply confirms it (at most `GROUND_MAX_TRIES` times). While idle the ground station sends a
signed **ping**, so the balloon knows the ground station is alive. The ping is sent in a time slot:
`GROUND_PING_DELAY_MS` (0.4 s) after the ground station hears the balloon's telemetry, when the
balloon is listening and not sending, so the two never talk at the same moment. If no telemetry is
heard, the ping repeats at the measured telemetry period and stays in its slot.

What you see on the ground:

- `telemetry #n ...`: fix, position, altitude, pressure, temperature, humidity, RSSI and SNR (every
  4 s, on its own). No cutdown state in it.
- `state ...`: state (`Safe` / `Armed` / `Firing`), one-time code, burn count and failsafe reason. The balloon sends it as the answer to a command or a poke, and on
  its own every `STATE_PERIOD_S` (30 s) and whenever it changes (armed, fired, back to safe, failsafe). The balloon returns to `Safe` by itself when the arm window (`ARM_WINDOW_MS`,
  15 s) ends; the ground station counts that window from the first `Armed` state it hears and follows with a `state Safe ... arm window over` line. To see the
  state sooner than the next report, `poke`.
- `!!! FAILSAFE CUTDOWN TRIGGERED ...`: the balloon cut itself down.

## Control panel

**Integrated RS41 + T-Beam dashboard:** see [PANEL_PL.md](PANEL_PL.md) for the
new headless Horus receiver, live plots, map, online landing predictions and
Fedora setup. The panel now defaults to decoding UDP audio on 127.0.0.1:7355
(48 kHz, Horus Binary 100 baud, 270 Hz spacing, callsign HAWK).
Use `--rs41-mode off` for the original T-Beam-only operation.

`scripts/panel.py` is a web page on your computer for the ground station: **ARM**, **FIRE** and
**DISARM** and **POKE** buttons, the balloon's latest telemetry, and the live log. It replaces the
serial monitor (only one program can use the port), so close `cargo run` / the monitor first:

```sh
pip install pyserial          # once
python3 scripts/panel.py --port /dev/ttyUSB0
```

Then open <http://127.0.0.1:8000>.

- FIRE is only enabled while the balloon reports ARMED (with a countdown of `ARM_WINDOW_MS`) and
  asks for confirmation. The ground station still refuses FIRE unless the balloon is armed.
- The log is saved to `logs/panel-<date>-<time>.log` (gitignored) and can be downloaded from the page.
- The page listens on `127.0.0.1` only, and every command needs a token that only the page gets,
  so other websites cannot press the buttons.
- `--http-port` changes the page's port, `--arm-window` the countdown (if you changed
  `ARM_WINDOW_MS`). Without `--port` it uses the only USB serial port it finds.
- Opening the port can restart the ground station on some boards; the panel keeps DTR/RTS low to
  avoid it.

## Saving logs

- **Ground station (on the computer):** the control panel saves its log (which includes the
  balloon's telemetry) to `logs/panel-<date>-<time>.log`, see above.
- **Balloon (on the board):** the log is also written to the SD card, one new file per boot
  (`L0001.TXT`, `L0002.TXT`, ...), flushed every 2 s, so the flight log can be read afterwards.
  Use a FAT32 card (the T-Beam's slot: SCK 36, MISO 37, MOSI 35, CS 47).

## Logs to CSV

`scripts/logs2csv.py` turns logs into a CSV with one row per telemetry packet, for a spreadsheet or
plotting. It takes ground (panel) logs and balloon (SD card) logs, mixed if you like:

```sh
python3 scripts/logs2csv.py logs/panel-*.log L0001.TXT -o flight.csv
```

Columns: `source` (`ground` / `balloon`), `file`, `time` (ground rows), `uptime_ms` (balloon rows),
`seq`, `fix`, `lat`, `lon`, `alt_m`, `sats` (balloon rows),
`pressure_hpa`, `temp_c`, `humidity_pct`, `rssi_dbm` and `snr_db` (ground rows). Ground rows are
the telemetry as the ground station received it, balloon rows what the balloon sent (including
packets the ground never heard). Without `-o` the CSV is printed.

## Failsafe

The balloon cuts itself down, and no command can cancel that. It fires only when **both** hold:

1. **Contact is lost**: no valid ping or command for `FAILSAFE_LOST_CONTACT_S` (900 s).
2. **It is high enough**: a fresh GPS altitude of at least `MIN_ALT_M` held for 5 s, **or** the GPS
   has no fix and `FAILSAFE_GPS_FAILED_AFTER_S` have passed since power-on.

### Packets

| Packet | Direction | Size | Magic | Protection |
|--------|-----------|------|-------|------------|
| Telemetry (position, air) | balloon -> ground | 33 | `0xB7` | 8-byte HMAC-SHA256 tag |
| State (answer to a command or poke) | balloon -> ground | 17 | `0xBA` | 8-byte HMAC-SHA256 tag |
| Ping | ground -> balloon | 12 | `0xA5` | 8-byte HMAC-SHA256 tag |
| Command | ground -> balloon | 15 | `0xC1` | 8-byte HMAC-SHA256 tag + one-time code |
| Poke | ground -> balloon | 12 | `0xA6` | 8-byte HMAC-SHA256 tag |
| Pong (answer to a poke) | balloon -> ground | 15 | `0xB9` | 8-byte HMAC-SHA256 tag |

The first byte tells the packet types apart. The exact byte layouts are documented at the top of
each source file.
