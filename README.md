# Temp Monitor

Jednoduchá aplikace pro monitorování teploty a vlhkosti z Bluetooth LE (BLE) senzoru, primárně určená pro **Thermopro TP357**. Aplikace je napsaná v jazyce Rust s využitím frameworku `egui`, zobrazuje aktuální data, jejich historii v grafu a ukládá je do CSV souboru.

## Stažení

Předkompilovanou verzi pro Windows (.exe) si můžete stáhnout přímo z tohoto repozitáře:
[**temp_monitor.exe**](https://github.com/crapper001/Thermopro-TP357-PC-app/raw/refs/heads/main/temp_monitor.exe)

## Funkce

-   **Skenování BLE zařízení**: Aplikace periodicky skenuje okolí a hledá konkrétní BLE senzor podle jeho MAC adresy.
-   **Zobrazení aktuálních dat**: Na přehledném panelu zobrazuje aktuální teplotu a vlhkost.
-   **Historický graf**: Vykresluje průběh teploty a vlhkosti v čase. Graf je možné přibližovat, oddalovat a posouvat.
-   **Statistiky**: Zobrazuje minimální a maximální naměřené hodnoty za dané období.
-   **Logování do CSV**: Všechny naměřené hodnoty automaticky ukládá do denních logů ve formátu `log_YYYY-MM-DD.csv`.
-   **Konfigurace**: Umožňuje uživatelské nastavení přes soubor `config.json`.
-   **Indikátory stavu**: Informuje o stavu skenování, času poslední aktualizace a síle signálu (RSSI).

## Jak to funguje

Aplikace běží ve dvou hlavních vláknech:
1.  **GUI vlákno**: Vykresluje uživatelské rozhraní pomocí `eframe/egui` a zpracovává interakce uživatele.
2.  **Bluetooth skener (asynchronní úloha)**: Běží na pozadí a pomocí knihovny `btleplug` vyhledává BLE zařízení. Když najde zařízení s cílovou MAC adresou, dekóduje jeho "manufacturer data", kde očekává data o teplotě a vlhkosti, a pošle je do hlavního vlákna ke zobrazení a uložení.

Data z BLE senzoru jsou dekódována specifickým způsobem – předpokládá se, že teplota a vlhkost jsou součástí tzv. "manufacturer data" v BLE inzerci (advertising packet).

## Konfigurace

Nastavení aplikace se nachází v souboru `config.json`. Pokud soubor neexistuje, vytvoří se s výchozími hodnotami při prvním spuštění.

```json
{
  "target_mac": "B8:59:CE:33:0F:93",
  "scan_timeout_secs": 15,
  "scan_pause_secs": 10,
  "temp_warn_high": 30.0,
  "temp_warn_low": 10.0
}
```

-   `target_mac`: MAC adresa vašeho BLE senzoru.
-   `scan_timeout_secs`: Jak dlouho (v sekundách) má trvat jedno skenování.
-   `scan_pause_secs`: Jak dlouho (v sekundách) má aplikace čekat mezi jednotlivými skenováními.
-   `temp_warn_high`: Horní hranice teploty, při jejímž překročení se hodnota zobrazí zlatou barvou.
-   `temp_warn_low`: Dolní hranice teploty, při jejímž poklesu se hodnota zobrazí modrou barvou.

## Logování dat

Aplikace ukládá data do CSV souboru s názvem ve formátu `log_ROK-MESIC-DEN.csv`. Každý den se vytvoří nový soubor. Data jsou ukládána v následujícím formátu se středníkem jako oddělovačem:

```
Datum;Cas;Teplota;Vlhkost
2024.09.03;15:30:00;25.5;45
```

Při spuštění aplikace načte historii z dnešního logovacího souboru, aby byl graf okamžitě zaplněn.

## Sestavení (Build)

Pro sestavení projektu z zdrojového kódu potřebujete mít nainstalovaný **Rust a Cargo**.

1.  Naklonujte repozitář:
    ```sh
    git clone <URL repozitáře>
    cd temp-monitor
    ```

2.  Spusťte sestavení v "release" módu (pro nejlepší výkon a skrytí konzole na Windows):
    ```sh
    cargo build --release
    ```

3.  Výsledný spustitelný soubor naleznete v adresáři `target/release/`.

## Použité knihovny

-   `eframe` a `egui`: Pro snadnou tvorbu grafického rozhraní.
-   `egui_plot`: Pro vykreslování grafů.
-   `btleplug`: Pro komunikaci s Bluetooth LE zařízeními.
-   `tokio`: Pro asynchronní operace (vyžadováno `btleplug`).
-   `serde`: Pro serializaci a deserializaci (načítání a ukládání `config.json`).
-   `csv`: Pro práci s CSV soubory.
-   `chrono`: Pro práci s časem a datem.
