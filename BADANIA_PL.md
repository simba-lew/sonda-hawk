# Badania gondoli: pomiary i kolejność

## Co oznaczają obecne dane

- Temperatura T-Beama: wnętrze gondoli, w miejscu zamontowania czujnika.
- Zewnętrzna temperatura RS41: otoczenie; czujnik musi znajdować się poza gondolą.
- Wewnętrzna temperatura RS41: osobny kanał diagnostyczny sondy, nie zamiennik
  temperatury gondoli ani akumulatora. Wykres jest domyślnie ukryty.
- Nieopisany kanał dodatkowej temperatury usunięto z kart i wykresów. Nadal pozostaje
  w surowych danych, aby nie tracić informacji z ramki.
- Napięcie RS41 jest diagnostyczne. Przed analizą trzeba sprawdzić skalowanie
  w firmware i porównać z multimetrem. Nie jest napięciem akumulatora T-Beama.

CSV zachowuje kolumnę `temperature` dla zgodności: fizyczne miejsce pomiaru zależy
od `source`. Dla T-Beama dodatkowo zapisuje się `internal_temperature`.

## Wykresy w panelu

Najważniejszy jest wspólny wykres temperatury: T-Beam wewnątrz, RS41 na zewnątrz.
Zakładki pojedynczych źródeł pokazują odpowiedni kanał. W porównaniu jest też
**ΔT = T_wewnątrz − T_otoczenia**. Dodatnia wartość oznacza cieplejsze wnętrze.
Próbki dobieramy według najbliższego czasu odbioru, z limitem 8 s. Nie interpolujemy
ani nie uzupełniamy braków. Czas odbioru nie gwarantuje identycznego czasu pomiaru
w dwóch urządzeniach; trzeba uwzględnić opóźnienie transmisji i bezwładność czujników.

Wszystkie wykresy czasowe używają wspólnego zakresu osi czasu. Pozostają wysokość,
prędkość pionowa, ciśnienie, wilgotność oraz SNR. Temperatura wewnętrzna RS41 i napięcie
są dostępne po włączeniu diagnostyki. SNR dwóch modemów służy do oceny każdego łącza,
nie do bezpośredniego porównania ich wydajności.

W kolejnej wersji warto dodać profil temperatury otoczenia względem wysokości,
tempo wychładzania w °C/min obliczane w dłuższym oknie oraz znaczniki początku próby,
włączenia grzania i zmiany obciążenia. Tych funkcji ta zmiana jeszcze nie dodaje.

## Priorytety badań

| Kolejność | Badanie | Co można zrobić teraz / czego brakuje |
|---|---|---|
| 1 | Wychładzanie wnętrza i czas osiągnięcia ustalonej temperatury granicznej | Obecne dwa pomiary temperatury. Granicę ustalić z wymaganiami konkretnych podzespołów. Czujniki w stałych miejscach. |
| 2 | Porównanie wariantów izolacji | Powtórzyć próbę z tym samym wnętrzem, obciążeniem, początkową temperaturą i warunkami otoczenia; zmieniać jeden element izolacji. |
| 3 | Praca elektroniki podczas wychładzania | Logować zaniki transmisji i restarty, a działanie podzespołów sprawdzać oddzielnie. Brak pakietu nie dowodzi awarii elektroniki. |
| 4 | Akumulator pod obciążeniem i po wychłodzeniu | Dodać pomiar temperatury ogniw, ich napięcia i prądu. Samego napięcia nie wystarczy do określenia dostępnej pojemności/energii. |
| 5 | Skuteczność ogrzewania | Potrzebne faktyczne ogrzewanie, zapis jego stanu i mocy oraz temperatura grzanego akumulatora/elektroniki. Porównać próby z grzaniem i bez niego. |
| 6 | Uwalnianie, serwa, silniki, ESC, złącza i mocowanie po wychłodzeniu | Wymagają osobnych prób funkcjonalnych i stanowiska. Czas, siła, moment lub ciąg wymagają odpowiednich pomiarów; nie wynikają z telemetrii temperatury. |
| 7 | Aerodynamika, drgania, stabilność, szczelność, oszronienie i dokładność IMU/barometru | Odpowiednio pomiar przepływu/sił, IMU, orientacja, pomiar różnicy ciśnień lub przecieku, kontrolowane oszronienie oraz przyrządy odniesienia. Obecny panel nie mierzy tych wielkości. |

Przy badaniu wpływu przepływu potrzebna jest zmierzona albo zadana i zweryfikowana
prędkość tunelu. Prędkość względem gruntu z GPS ani prędkość wznoszenia nie są
prędkością opływu gondoli. Nie można więc wyznaczać z nich oporu czy konwekcji.

## Pierwsza seria prób

1. Sprawdzić wskazania obu czujników obok siebie w stabilnych warunkach i zanotować
   różnicę; nie korygować automatycznie odczytów bez ustalenia przyczyny.
2. Zmierzyć wychładzanie gondoli przy stałym obciążeniu. Zapisać temperaturę
   początkową, rozmieszczenie czujników, wariant izolacji i warunki tunelu.
3. Powtórzyć dla kolejnego wariantu izolacji przy możliwie takich samych warunkach.
   Porównać przebieg temperatur i czas dojścia do tej samej granicy; wykonać powtórzenia.

ΔT i tempo wychładzania pozwalają porównywać konkretne konfiguracje, ale same
nie dają przewodności izolacji ani współczynnika przenikania ciepła. Potrzebny jest
bilans energii: m.in. moc tracona wewnątrz, geometria, pojemność cieplna i warunki przepływu.
