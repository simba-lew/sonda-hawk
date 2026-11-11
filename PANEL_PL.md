# Wspólny panel RS41 + T-Beam

Uruchamiasz SDRangela oraz **jeden skrypt panelu**. Panel sam uruchamia dekoder
Horusa w tle; Horus GUI nie jest potrzebne. Zmiany dotyczą programu na komputerze,
nie wymagają przeprogramowania T-Beama ani RS41.

## Instalacja na Fedorze

Z katalogu rozpakowanego projektu:

```bash
sudo dnf install python3 python3-pip python3-devel gcc make libffi-devel
python3 -m venv .venv
source .venv/bin/activate
python -m pip install -r scripts/requirements-panel.txt
```

Instalacja wymaga internetu. `horusdemodlib` zawiera modem napisany w C;
kompilator i nagłówki mogą być potrzebne przy instalacji na Fedorze.
Przetestowane tutaj z Pythonem 3.12 i horusdemodlib 0.6.2. Zależność deklaruje
obsługę Pythona od 3.9, ale instalację na Twojej wersji Fedory trzeba sprawdzić.

## Uruchomienie

1. W SDRangel pozostaw działający odbiór i wyjście dźwięku UDP:
   **127.0.0.1:7355, mono, signed PCM 16-bit little-endian, 48000 Hz**.
2. Zamknij Horus GUI i monitor portu szeregowego.
3. Podłącz naziemnego T-Beama przez USB.
4. Uruchom:

```bash
source .venv/bin/activate
python scripts/panel.py --port /dev/ttyUSB0
```

Otwórz **http://127.0.0.1:8000**. Port może nazywać się też `/dev/ttyACM0`;
przy jednym porcie USB można pominąć `--port`. USB pracuje przy 115200 bodach.
Domyślny filtr RS41 to **HAWK**, zgodnie ze zrzutem Horusa.

Parametry dekodera odpowiadają ustawieniom ze zrzutu: Horus Binary v1/v2/v3,
100 bodów, estymacja maskowa, odstęp tonów 270 Hz, granice estymatora 100–4000 Hz.
Dekoder ma wyłączność na port audio: Horus GUI nie może równolegle odbierać na 7355.

Jeśli poziom wejściowy wynosi blisko 0 dBFS i Horus pokazywał „TOO HIGH”,
zmniejsz wzmocnienie wyjścia dźwięku w SDRangel. Nie zakładaj konkretnej wartości
wzmocnienia: obserwuj wskaźnik szczytu audio i skuteczność dekodowania.

Przy braku dostępu do USB sprawdź uprawnienia portu, np. `ls -l /dev/ttyUSB0`.
Nie uruchamiaj panelu przez sudo. Port i jego uprawnienia zależą od konfiguracji systemu.

## Co pokazuje panel

Panel ma trzy zakładki: **T-Beam**, **RS41** i **Porównanie / nakładanie**.
Każda zakładka pojedynczego źródła pokazuje jego odczyty, wykresy, ślad na mapie
i osobny surowy log. Komendy są w zakładce T-Beam. W porównaniu można niezależnie
włączać serie T-Beama i RS41 na wspólnych osiach oraz mapie. Oba źródła odbierają
i zapisują dane także wtedy, kiedy ich zakładka jest ukryta.

Log T-Beama (`/log`) zawiera linie portu USB, a log RS41 (`/rs41.log`) ramki HEX,
wynik dekodera JSON i jego komunikaty. Są osobne przyciski pobierania i przewijania.
Plik RS41 ma nazwę `logs/panel-*-rs41.log`. Log zapisuje również powtórzone ramki,
choć historia wykresu odrzuca duplikaty. Tryb demo oznacza swoje logi jako syntetyczne
i nie udaje prawdziwych ramek HEX. Logi dotyczą zdekodowanych ramek, nie nagrania audio/IQ.

- Dotychczasowe komendy ARM, FIRE, DISARM, POKE oraz log stacji naziemnej.
- Oddzielne odczyty RS41 i T-Beama, czas od ostatniego pakietu i status audio.
- Dziewięć wykresów: wysokość, temperatura zewnętrzna, ciśnienie, wilgotność,
  prędkość pionowa, SNR, temperatura wewnętrzna RS41, temperatura dodatkowa RS41 i napięcie.
- Pozycje i ślady obu źródeł. „Włącz podkład mapy” pobiera kafelki OpenStreetMap;
  bez internetu nadal widać ślady na pustym podkładzie. Nie ma pobierania map na zapas.
- Wykresy i biblioteka mapy są lokalne, bez zależności od CDN przy uruchomieniu.
- Automatyczny zapis do `logs/panel-*.log`, `*.csv` i `*.jsonl`.
  Surowa ramka RS41 jest zapisywana jako `raw_hex` w JSONL.
- Obliczone predykcje trafiają do osobnego `*-predictions.jsonl`.

W pamięci pozostaje ostatnie 20000 pakietów każdego źródła. Pełna sesja pozostaje
w plikach. Odświeżenie strony przywraca historię bieżącego procesu; restart panelu
zaczyna nową sesję. Ta wersja nie ma jeszcze odtwarzania archiwalnego lotu w panelu.

Przy zerowej liczbie satelitów pozycja RS41 nie trafia do śladu ani do predykcji.
Na Twoim zrzucie Horus pokazuje **0 satelitów**, mimo wyświetlanych współrzędnych.
Sprawdź odbiór GPS na zewnątrz. Pozostałe pomiary nadal będą wyświetlane.
Wartości napięcia wyświetlamy dokładnie według dekodera: odczyt około 0.04 V
widoczny na zrzucie może wymagać sprawdzenia jednostek w firmware RS41.

## Predykcja z internetu

Wybierz źródło pozycji i fazę lotu, ustaw wysokość pęknięcia [m AMSL], prędkość
wznoszenia [m/s] oraz **prędkość opadania na poziomie morza** [m/s], potem kliknij
„Oblicz predykcję”. Początkowe 30000 m i 5 m/s są przykładami, nie parametrami
Twojego balonu. Prędkość pionowa z telemetrii jest pokazywana obok, lecz nie
nadpisuje samoczynnie parametrów modelu.

Opcja „Co 2 minuty” odświeża predykcję dopóki strona jest otwarta. Potrzebna jest
poprawna pozycja odebrana w ostatnich 30 s. Serwer wykonuje obliczenie w osobnym
wątku i po 25 s zgłasza timeout; USB i wykresy działają dalej. Po błędzie pozostaje
ostatnia trasa z datą obliczenia. ETA na stronie jest w lokalnej strefie przeglądarki;
zapytanie do modelu używa UTC. Sprawdź zegar komputera.

Usługa: https://api.v2.sondehub.org/tawhiri — wiatr z modelu NOAA GFS.
Zapytanie wysyła pozycję, wysokość, czas i parametry lotu do SondeHub; nie włącza
publikowania całej telemetrii w trackerze. Usługa publiczna jest przeznaczona do
zastosowań niekomercyjnych: https://predict.sondehub.org/ .
Predykcja jest pojedynczym oszacowaniem, nie obszarem pewności.

Dla fazy opadania stosujemy standardowy profil Tawhiri z umownym pęknięciem
1 m nad aktualną wysokością i krótkim wznoszeniem 0.1 s. Na mapie pokazujemy
tylko fazę opadania. To przybliżenie, podobne do rozwiązania w ChaseMapper.
Wartość prędkości opadania nadal odnosi się do poziomu morza.

## Przydatne tryby

### Błąd „Address already in use”

Port 8000 jest używany przez stronę panelu, a UDP 7355 przez dekoder audio.
Zatrzymaj poprzedni panel przez Ctrl+C i zamknij Horus GUI przed odbiorem audio
w nowym panelu. Zamknięcie samej karty przeglądarki nie zatrzymuje serwera.

Procesy zajmujące porty można sprawdzić poleceniami:

```bash
ss -ltnp 'sport = :8000'
ss -lunp 'sport = :7355'
```

Inny port strony można wybrać przez `--http-port 8001`, ale to nie rozwiązuje
zajętości portu audio ani portu USB. Tryb `sonda-demo --lan --http-port 8001`
nie korzysta z audio ani USB i nadaje się do oddzielnego sprawdzenia strony.
Panel rezerwuje port HTTP przed uruchomieniem dekodera i USB; gdy jest zajęty,
kończy działanie czytelnym komunikatem.

```bash
# Syntetyczny lot; bez sprzętu i bez możliwości wysyłania komend:
python scripts/panel.py --demo

# Tylko RS41, bez stacji USB:
python scripts/panel.py --no-serial

# Tylko T-Beam:
python scripts/panel.py --port /dev/ttyUSB0 --rs41-mode off

# Inny znak sondy:
python scripts/panel.py --port /dev/ttyUSB0 --callsign TWOJZNAK

# Tryb porównawczy: pozostaw Horus GUI i odbieraj jego JSON UDP:
python scripts/panel.py --port /dev/ttyUSB0 --rs41-mode summary
```

W trybie `summary` w Horus GUI / Other włącz „Horus UDP Output”, port **55672**.
Dźwięk na 7355 nadal odbiera wtedy Horus GUI. UDP summary może zawierać mniej
pól niż pełny dekoder; brakujące pomiary pokazują „—”. `--callsign ''` przyjmuje
dowolny znak, ale panel ma jedną serię RS41 — stosuj filtr przy wielu sondach.

Domyślnie panel nasłuchuje wyłącznie na localhost. Opcja `--lan` włącza dostęp
po IPv4 w sieci lokalnej i wypisuje adresy laptopa. Urządzenia mające dostęp do strony
mogą również wydawać komendy, dlatego jest to tryb dla zaufanej sieci.
Przykłady i konfiguracja zapory Fedory: sekcja „Dostęp przez Wi-Fi / LAN” w README.
Zachowano token komend, kontrolę Host/Origin i potwierdzenie FIRE
z dotychczasowego panelu. Firmware, uwierzytelnianie radiowe, automatyczne pingi
i failsafe nie zostały zmienione.

## Weryfikacja

```bash
python -m unittest discover -s scripts/tests -v
```

Testy obejmują parsowanie T-Beama, prawdziwe ramki Horus v3 i ich jednostki,
historię/CSV, brak fixa, API HTTP z wirtualnym portem szeregowym, dekoder audio
z syntetycznym PCM oraz obsługę powodzenia/błędu predykcji na odpowiedzi testowej.
Do końcowego sprawdzenia na Twoim sprzęcie pozostaje rzeczywisty odbiór RF,
wyświetlanie w przeglądarce oraz działanie zainstalowanego sterownika USB.
Test DOM z prawdziwym kodem Leaflet sprawdził odczyty obu źródeł, dziewięć wykresów
i stany predykcji. Rysowanie canvas było zastąpione atrapą, więc ten test nie
potwierdza wyglądu. Chromium nie uruchomiło się w środowisku testowym.
Wykonano też udane zapytanie do rzeczywistej usługi predykcji SondeHub.

Kod integracji korzysta z API:
https://github.com/projecthorus/horusdemodlib
oraz https://tawhiri.readthedocs.io/en/latest/api.html .
Mapa: Leaflet 1.9.4, licencja BSD-2-Clause w `scripts/web/LEAFLET-LICENSE.txt`.
