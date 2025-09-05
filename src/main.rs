// Atribut pro skrytí konzolového okna ve finální verzi (v release buildu)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// --- Importy ---
use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral, ScanFilter};
use btleplug::platform::Manager;
use chrono::{DateTime, Local, NaiveDateTime};
use eframe::egui;
use egui_extras::{StripBuilder, Size};
// OPRAVA: Odstraněn nepoužívaný PlotPoint
use egui_plot::PlotMemory;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fs;
#[cfg(debug_assertions)]
use std::io::Write;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use log::{info, warn, error, debug};

// --- Konstanty a Konfigurace ---
const MAX_HISTORY_POINTS: usize = 200;
const CONFIG_FILE: &str = "config.json";

// --- DATOVÉ STRUKTURY ---

#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct Config {
    target_mac: String,
    scan_timeout_secs: u64,
    scan_pause_secs: u64,
    duplicate_threshold_secs: u64,
    temp_warn_high: f32,
    temp_warn_low: f32,
    continuous_mode: bool,
    load_all_history: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            target_mac: "B8:59:CE:33:0F:93".to_string(),
            scan_timeout_secs: 20,
            scan_pause_secs: 20,
            duplicate_threshold_secs: 30,
            temp_warn_high: 30.0,
            temp_warn_low: 10.0,
            continuous_mode: true,
            load_all_history: true,
        }
    }
}

#[derive(Clone, Debug)]
struct HistoryPoint { timestamp: DateTime<Local>, temp: f32, hum: u8, }
#[derive(Clone, Debug)]
struct BleDataPoint { timestamp: DateTime<Local>, temp: f32, hum: u8, device_id: String, rssi: Option<i16>, raw_data: Vec<u8>, }
enum AppMessage { NewData(BleDataPoint), StatusUpdate(String), CsvWriteStatus(bool), }

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct TempMonitorApp {
    config: Config,
    settings_open: bool,
    #[serde(skip)] rx: mpsc::Receiver<AppMessage>,
    #[serde(skip)] shared_config: Arc<Mutex<Config>>,
    #[serde(skip)] history: VecDeque<HistoryPoint>,
    #[serde(skip)] last_data_point: Option<BleDataPoint>,
    #[serde(skip)] last_csv_write_ok: bool,
    #[serde(skip)] scan_status: String,
    #[serde(skip)] zoom_factor: f32,
    #[serde(skip)] reset_plot: bool,
    #[serde(skip)] background_processor: Option<thread::JoinHandle<()>>,
    #[serde(skip)] config_changed: bool,
    #[serde(skip)] toast_message: Option<(String, Instant)>,
}

impl Default for TempMonitorApp {
    fn default() -> Self {
        let (_tx, rx) = mpsc::channel();
        Self {
            config: load_config(), settings_open: false, rx, shared_config: Arc::new(Mutex::new(Config::default())),
            history: VecDeque::new(), last_data_point: None, last_csv_write_ok: true, scan_status: "Inicializace...".to_string(),
            zoom_factor: 1.0, reset_plot: false, background_processor: None, config_changed: false,
            toast_message: None,
        }
    }
}

impl TempMonitorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        info!("Vytváření nové instance aplikace TempMonitorApp.");
        let mut app: Self = if let Some(storage) = cc.storage { eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default() } else { Default::default() };
        let (gui_tx, gui_rx) = mpsc::channel(); let (scanner_tx, processor_rx) = mpsc::channel();
        app.rx = gui_rx;
        let shared_config = Arc::new(Mutex::new(app.config.clone()));
        app.shared_config = shared_config.clone();
        let processor_shared_config = shared_config.clone();
        let processor = thread::spawn(move || { background_data_processor(processor_rx, gui_tx, processor_shared_config); });
        app.background_processor = Some(processor);
        info!("Spouštím Bluetooth scanner v asynchronním vlákně.");
        let rt = tokio::runtime::Runtime::new().expect("Nelze vytvořit Tokio runtime");
        rt.spawn(bluetooth_scanner(scanner_tx, shared_config));
        std::mem::forget(rt);
        app.history = load_history_from_csv();
        app
    }

    fn add_data_point(&mut self, data: BleDataPoint) {
        debug!("Aktualizuji UI s novým datovým bodem: {:?}", data);
        let limit = if self.config.load_all_history { usize::MAX } else { MAX_HISTORY_POINTS };
        while self.history.len() >= limit { self.history.pop_front(); }
        let history_point = HistoryPoint { timestamp: data.timestamp, temp: data.temp, hum: data.hum };
        self.history.push_back(history_point);
        self.last_data_point = Some(data);
    }
}

// --- Logika GUI ---
impl eframe::App for TempMonitorApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
        if self.config_changed {
            info!("Změna v konfiguraci detekována, ukládám do souboru.");
            save_config(&self.config);
            self.config_changed = false;
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_secs(1));
        while let Ok(message) = self.rx.try_recv() {
            match message {
                AppMessage::NewData(data_point) => self.add_data_point(data_point),
                AppMessage::StatusUpdate(status) => { debug!("Aktualizace stavu skeneru: {}", status); self.scan_status = status; },
                AppMessage::CsvWriteStatus(ok) => self.last_csv_write_ok = ok,
            }
        }
        let mut visual = egui::Visuals::dark();
        visual.window_fill = egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240);
        ctx.set_visuals(visual);
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Soubor", |ui| {
                    if ui.button("Nastavení").clicked() { self.settings_open = true; ui.close_menu(); }
                    if ui.button("Ukončit").clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
                });
                ui.separator();
                if ui.button("➖").on_hover_text("Oddálit").clicked() { self.zoom_factor = 0.7; }
                if ui.button("➕").on_hover_text("Přiblížit").clicked() { self.zoom_factor = 1.25; }
                if ui.button("⛶").on_hover_text("Vycentrovat graf").clicked() { self.reset_plot = true; }
            });
        });
        if self.reset_plot { info!("Resetuji pohled grafů."); ctx.memory_mut(|memory| { memory.data.remove::<PlotMemory>(egui::Id::new("linked_plots")); }); }
        
        egui::CentralPanel::default().show(ctx, |ui| {
            StripBuilder::new(ui)
                .size(Size::relative(0.10)).size(Size::relative(0.425)).size(Size::relative(0.425)).size(Size::relative(0.05))
                .vertical(|mut strip| {
                    strip.cell(|ui| { ui.columns(4, |columns| {
                        columns[0].vertical_centered(|ui| draw_temperature_info(ui, &self.history, &self.config));
                        columns[1].vertical_centered(|ui| draw_humidity_info(ui, &self.history));
                        columns[2].vertical(|ui| draw_scan_metadata(ui, &self.last_data_point, &self.scan_status));
                        columns[3].vertical(|ui| draw_data_details(ui, &self.last_data_point, self.last_csv_write_ok));
                    });});
                    strip.cell(|ui| { ui.label(egui::RichText::new("Teplota").strong()); draw_temperature_graph(self, ui, ctx); });
                    strip.cell(|ui| { ui.label(egui::RichText::new("Vlhkost").strong()); draw_humidity_graph(self, ui, ctx); });
                    strip.cell(|ui| { ui.separator(); ui.vertical_centered(|ui| { ui.horizontal_centered(|ui| { ui.label("Autorem aplikace je Soběslav Holec"); });});});
                });
        });

        if let Some((message, created_at)) = &self.toast_message {
            egui::Area::new("toast_area".into())
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -20.0))
                .show(ctx, |ui| {
                    let frame = egui::Frame::popup(ui.style());
                    frame.show(ui, |ui| { ui.label(message); });
                });
            if created_at.elapsed() > Duration::from_secs(3) {
                self.toast_message = None;
            }
        }

        if self.zoom_factor != 1.0 { self.zoom_factor = 1.0; }
        if self.reset_plot { self.reset_plot = false; }
        self.draw_settings_window(ctx);
    }
}

impl TempMonitorApp {
    fn draw_settings_window(&mut self, ctx: &egui::Context) {
        if self.settings_open {
            let mut is_open = self.settings_open;
            let old_config = self.config.clone();
            egui::Window::new("Nastavení").open(&mut is_open).show(ctx, |ui| {
                ui.label("Cílová MAC adresa:"); ui.text_edit_singleline(&mut self.config.target_mac);
                ui.separator();
                ui.add(egui::DragValue::new(&mut self.config.scan_timeout_secs).prefix("Timeout skenování (s): "));
                ui.add(egui::DragValue::new(&mut self.config.scan_pause_secs).prefix("Pauza mezi skeny (s): "));
                ui.separator();
                ui.add(egui::DragValue::new(&mut self.config.duplicate_threshold_secs).prefix("Interval pro duplikáty (s): "));
                ui.label("Záznamy ze stejného zařízení budou ignorovány po tuto dobu.");
                ui.separator();
                ui.checkbox(&mut self.config.continuous_mode, "Kontinuální režim");
                ui.label("⚠️ Kontinuální režim pouze zrychluje skenování, stále platí interval pro duplikáty.");
                ui.separator();
                ui.checkbox(&mut self.config.load_all_history, "Načíst kompletní historii z CSV při startu");
                ui.label("⚠️ Restartujte aplikaci, aby se změna projevila.");
                if self.config.load_all_history { ui.label(egui::RichText::new("POZOR: Může zpomalit start.").color(egui::Color32::YELLOW)); }
                ui.separator();
                ui.add(egui::DragValue::new(&mut self.config.temp_warn_high).prefix("Mez pro varování (°C): ").speed(0.1));
                ui.add(egui::DragValue::new(&mut self.config.temp_warn_low).prefix("Spodní mez (°C): ").speed(0.1));
            });
            if !is_open || self.config != old_config {
                if self.config != old_config { info!("Detekována změna v nastavení."); self.config_changed = true; }
                if let Ok(mut shared) = self.shared_config.lock() { *shared = self.config.clone(); debug!("Sdílená konfigurace byla aktualizována."); }
            }
            self.settings_open = is_open;
        }
    }
}

// --- Vykreslovací funkce ---

fn draw_temperature_graph(app: &mut TempMonitorApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    use egui_plot::{GridMark, Line, Plot, Points, MarkerShape, PlotPoints};
    let temp_data_points: Vec<[f64; 2]> = app.history.iter().map(|p| [p.timestamp.timestamp() as f64, p.temp as f64]).collect();
    let temp_line = Line::new(PlotPoints::new(temp_data_points.clone())).color(egui::Color32::from_rgb(255, 100, 100)).width(2.0);
    let temp_points = Points::new(PlotPoints::new(temp_data_points)).shape(MarkerShape::Circle).radius(3.0).color(egui::Color32::from_rgb(0, 255, 0)).highlight(true);

    let mut plot = Plot::new("temperature_plot").height(ui.available_height()).width(ui.available_width())
        .link_axis(egui::Id::new("linked_plots"), true, false).show_background(false).allow_drag(true).allow_zoom(true)
        .auto_bounds(egui::Vec2b::new(true, true)).show_x(false)
        .label_formatter(|_name, value| { let time = DateTime::from_timestamp(value.x as i64, 0).unwrap_or_default().with_timezone(&Local); format!("Čas: {}\nTeplota: {:.1}°C", time.format("%H:%M:%S"), value.y) })
        .x_axis_formatter(|mark: GridMark, _, _| { let time = DateTime::from_timestamp(mark.value as i64, 0).unwrap_or_default().with_timezone(&Local); time.format("%H:%M").to_string() })
        .y_axis_formatter(|mark: GridMark, _, _| format!("{:.1}°C", mark.value));
    if app.reset_plot { plot = plot.reset(); }
    if let (Some(min), Some(max)) = (app.history.iter().map(|p| p.temp).min_by(|a, b| a.partial_cmp(b).unwrap()), app.history.iter().map(|p| p.temp).max_by(|a, b| a.partial_cmp(b).unwrap())) {
        if (max - min).abs() < f32::EPSILON { plot = plot.include_y(min - 0.5).include_y(max + 0.5); }
    }

    // OPRAVA: Výsledek se už neukládá do proměnné
    plot.show(ui, |plot_ui| {
        plot_ui.line(temp_line);
        plot_ui.points(temp_points);
        if app.zoom_factor != 1.0 { plot_ui.zoom_bounds(egui::vec2(app.zoom_factor, app.zoom_factor), plot_ui.plot_bounds().center()); }
        
        if plot_ui.response().clicked() {
            if let Some(pos) = plot_ui.pointer_coordinate() {
                let closest_point = app.history.iter().min_by_key(|p| (p.timestamp.timestamp() as f64 - pos.x).abs() as u64);
                if let Some(point) = closest_point {
                    if (point.temp as f64 - pos.y).abs() < 1.0 {
                        let text_to_copy = format!("Čas: {}, Teplota: {:.1}°C", point.timestamp.format("%H:%M:%S"), point.temp);
                        ctx.output_mut(|o| o.copied_text = text_to_copy.clone());
                        app.toast_message = Some(("Zkopírováno do schránky!".to_owned(), Instant::now()));
                        info!("Zkopírováno do schránky: {}", text_to_copy);
                    }
                }
            }
        }
    });
}

fn draw_humidity_graph(app: &mut TempMonitorApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    use egui_plot::{GridMark, Line, Plot, Points, MarkerShape, PlotPoints};
    let hum_data_points: Vec<_> = app.history.iter().map(|p| [p.timestamp.timestamp() as f64, p.hum as f64]).collect();
    let hum_line = Line::new(PlotPoints::new(hum_data_points.clone())).color(egui::Color32::from_rgb(100, 100, 255)).width(2.0);
    let hum_points = Points::new(PlotPoints::new(hum_data_points)).shape(MarkerShape::Circle).radius(3.0).color(egui::Color32::from_rgb(0, 255, 0)).highlight(true);

    let mut plot = Plot::new("humidity_plot").height(ui.available_height()).width(ui.available_width())
        .link_axis(egui::Id::new("linked_plots"), true, false).show_background(false).allow_drag(true).allow_zoom(true)
        .auto_bounds(egui::Vec2b::new(true, true)).show_axes([true, true])
        .label_formatter(|_name, value| { let time = DateTime::from_timestamp(value.x as i64, 0).unwrap_or_default().with_timezone(&Local); format!("Čas: {}\nVlhkost: {:.0}%", time.format("%H:%M:%S"), value.y) })
        .x_axis_formatter(|mark: GridMark, _, _| { let time = DateTime::from_timestamp(mark.value as i64, 0).unwrap_or_default().with_timezone(&Local); time.format("%H:%M").to_string() })
        .y_axis_formatter(|mark: GridMark, _, _| format!("{:.0}%", mark.value));
    if app.reset_plot { plot = plot.reset(); }
    if let (Some(min), Some(max)) = (app.history.iter().map(|p| p.hum).min(), app.history.iter().map(|p| p.hum).max()) {
        if min == max { plot = plot.include_y(min as f64 - 1.0).include_y(max as f64 + 1.0); }
    }
    
    plot.show(ui, |plot_ui| {
        plot_ui.line(hum_line);
        plot_ui.points(hum_points);
        if app.zoom_factor != 1.0 { plot_ui.zoom_bounds(egui::vec2(app.zoom_factor, app.zoom_factor), plot_ui.plot_bounds().center()); }
        
        if plot_ui.response().clicked() {
            if let Some(pos) = plot_ui.pointer_coordinate() {
                let closest_point = app.history.iter().min_by_key(|p| (p.timestamp.timestamp() as f64 - pos.x).abs() as u64);
                if let Some(point) = closest_point {
                    if (point.hum as f64 - pos.y).abs() < 2.0 {
                        let text_to_copy = format!("Čas: {}, Vlhkost: {}%", point.timestamp.format("%H:%M:%S"), point.hum);
                        ctx.output_mut(|o| o.copied_text = text_to_copy.clone());
                        app.toast_message = Some(("Zkopírováno do schránky!".to_owned(), Instant::now()));
                        info!("Zkopírováno do schránky: {}", text_to_copy);
                    }
                }
            }
        }
    });
}


// --- I/O, logovací a background funkce ---
// (zde je zbytek kódu, který se nemění)
// ...
fn get_daily_log_filename() -> String { Local::now().format("log_%Y-%m-%d.csv").to_string() }
fn draw_temperature_info(ui: &mut egui::Ui, history: &VecDeque<HistoryPoint>, config: &Config) {
    let temp_min = history.iter().map(|p| p.temp).min_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal)).unwrap_or(0.0);
    let temp_max = history.iter().map(|p| p.temp).max_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal)).unwrap_or(0.0);
    ui.label(egui::RichText::new("Teplota").color(egui::Color32::GRAY));
    if let Some(point) = history.back() {
        let current_temp = point.temp;
        let mut color = egui::Color32::from_rgb(255, 100, 100);
        if current_temp > config.temp_warn_high { color = egui::Color32::GOLD; } else if current_temp < config.temp_warn_low { color = egui::Color32::from_rgb(120, 180, 255); }
        ui.label(egui::RichText::new(format!("{:.1}°C", current_temp)).size(32.0).color(color));
    } else { ui.label(egui::RichText::new("N/A").size(32.0)); }
    ui.label(format!("Min: {:.1}° / Max: {:.1}°", temp_min, temp_max));
}

fn draw_humidity_info(ui: &mut egui::Ui, history: &VecDeque<HistoryPoint>) {
    let hum_min = history.iter().map(|p| p.hum).min().unwrap_or(0);
    let hum_max = history.iter().map(|p| p.hum).max().unwrap_or(0);
    ui.label(egui::RichText::new("Vlhkost").color(egui::Color32::GRAY));
    if let Some(point) = history.back() {
        ui.label(egui::RichText::new(format!("{}%", point.hum)).size(32.0).color(egui::Color32::from_rgb(100, 100, 255)));
    } else { ui.label(egui::RichText::new("N/A").size(32.0)); }
    ui.label(format!("Min: {}% / Max: {}%", hum_min, hum_max));
}

fn draw_scan_metadata(ui: &mut egui::Ui, last_data: &Option<BleDataPoint>, status: &str) {
    ui.horizontal(|ui| { ui.label(egui::RichText::new("Stav:").color(egui::Color32::GRAY)); ui.label(status); });
    if let Some(data) = last_data {
        ui.horizontal(|ui| { ui.label(egui::RichText::new("Aktualizace:").color(egui::Color32::GRAY)); ui.label(data.timestamp.format("%H:%M:%S").to_string()); });
        ui.horizontal(|ui| { ui.label(egui::RichText::new("RSSI:").color(egui::Color32::GRAY)); if let Some(rssi) = data.rssi { ui.label(format!("{} dBm", rssi)); } else { ui.label("N/A"); }});
    }
}

fn draw_data_details(ui: &mut egui::Ui, last_data: &Option<BleDataPoint>, csv_ok: bool) {
    if let Some(data) = last_data {
        ui.horizontal(|ui| { ui.label(egui::RichText::new("ID Zařízení:").color(egui::Color32::GRAY)); ui.label(data.device_id.to_string()); });
        ui.horizontal(|ui| { ui.label(egui::RichText::new("Raw data:").color(egui::Color32::GRAY)); ui.label(data.raw_data.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" ")); });
        ui.horizontal(|ui| { ui.label(egui::RichText::new("Zápis CSV:").color(egui::Color32::GRAY)); if csv_ok { ui.label(egui::RichText::new("OK").color(egui::Color32::GREEN)); } else { ui.label(egui::RichText::new("Chyba").color(egui::Color32::RED)); } });
    }
}

fn log_to_csv(temp: f32, hum: u8) -> Result<(), csv::Error> {
    let filename = get_daily_log_filename(); let file_exists = Path::new(&filename).exists();
    let file = fs::OpenOptions::new().append(true).create(true).open(filename)?;
    let mut wtr = csv::WriterBuilder::new().delimiter(b';').from_writer(file);
    if !file_exists { wtr.write_record(&["Datum", "Cas", "Teplota", "Vlhkost"])?; }
    let now = Local::now(); let temp_str = format!("{:.1}", temp).replace('.', ",");
    wtr.write_record(&[ now.format("%Y.%m.%d").to_string(), now.format("%H:%M:%S").to_string(), temp_str, hum.to_string() ])?;
    wtr.flush()?; Ok(())
}

fn load_history_from_csv() -> VecDeque<HistoryPoint> {
    let config = load_config();
    info!("Načítám historii z CSV. Načíst vše: {}", config.load_all_history);
    let capacity = if config.load_all_history { 0 } else { MAX_HISTORY_POINTS };
    let mut history = VecDeque::with_capacity(capacity);
    let filename = get_daily_log_filename();
    if let Ok(file) = fs::File::open(&filename) {
        let mut rdr = csv::ReaderBuilder::new().delimiter(b';').from_reader(file);
        let all_records: Vec<_> = rdr.records().filter_map(Result::ok).collect();
        info!("Nalezeno {} záznamů v souboru '{}'.", all_records.len(), filename);
        let records_to_load: Box<dyn Iterator<Item = &csv::StringRecord>> = if config.load_all_history {
            Box::new(all_records.iter())
        } else {
            let start_index = all_records.len().saturating_sub(MAX_HISTORY_POINTS);
            Box::new(all_records.iter().skip(start_index))
        };
        for result in records_to_load {
            if let (Some(date_str), Some(time_str), Some(temp_str), Some(hum_str)) = (result.get(0), result.get(1), result.get(2), result.get(3)) {
                let datetime_str = format!("{} {}", date_str, time_str);
                if let Ok(naive_dt) = NaiveDateTime::parse_from_str(&datetime_str, "%Y.%m.%d %H:%M:%S") {
                    if let (Ok(temp), Ok(hum)) = (temp_str.replace(',', ".").parse(), hum_str.parse()) {
                        history.push_back(HistoryPoint { timestamp: naive_dt.and_local_timezone(Local).unwrap(), temp, hum });
                    }
                }
            }
        }
        info!("Načteno {} bodů do historie grafu.", history.len());
    } else { warn!("Soubor s historií '{}' nenalezen.", filename); }
    history
}

fn load_config() -> Config {
    info!("Načítám konfiguraci z '{}'.", CONFIG_FILE);
    fs::read_to_string(CONFIG_FILE).ok().and_then(|c| serde_json::from_str::<Config>(&c).ok()).unwrap_or_default()
}
fn save_config(config: &Config) {
    if let Ok(content) = serde_json::to_string_pretty(config) { let _ = fs::write(CONFIG_FILE, content); }
}

fn background_data_processor(rx: mpsc::Receiver<AppMessage>, tx: mpsc::Sender<AppMessage>, shared_config: Arc<Mutex<Config>>) {
    info!("Spouštím background procesor pro data.");
    let mut last_save_time: Option<Instant> = None;
    for received in rx {
        match received {
            AppMessage::NewData(data_point) => {
                let config = shared_config.lock().unwrap().clone();
                let now = Instant::now();
                let should_save = last_save_time.map_or(true, |last| {
                    now.duration_since(last).as_secs() >= config.duplicate_threshold_secs
                });
                if should_save {
                    info!("Zapisuji data do CSV: teplota={}, vlhkost={}", data_point.temp, data_point.hum);
                    let write_ok = log_to_csv(data_point.temp, data_point.hum).is_ok();
                    if !write_ok { error!("Nepodařilo se zapsat do CSV souboru!"); }
                    let _ = tx.send(AppMessage::CsvWriteStatus(write_ok));
                    last_save_time = Some(now);
                    if tx.send(AppMessage::NewData(data_point)).is_err() { error!("GUI kanál je uzavřen, ukončuji background procesor."); break; }
                } else {
                    debug!("Přeskakuji zápis i zobrazení v grafu (duplikát).");
                }
            },
            AppMessage::StatusUpdate(status) => {
                if tx.send(AppMessage::StatusUpdate(status)).is_err() { error!("GUI kanál je uzavřen, ukončuji background procesor."); break; }
            },
            _ => {}
        }
    }
    info!("Background procesor ukončen.");
}


fn main() -> Result<(), eframe::Error> {
    #[cfg(debug_assertions)]
    env_logger::Builder::new()
        .format(|buf, record| { writeln!(buf, "[{}] [{}] - {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), record.level(), record.args()) })
        .filter(None, log::LevelFilter::Info)
        .init();
    info!("Logger inicializován, spouštím aplikaci...");
    let viewport = egui::ViewportBuilder::default().with_inner_size([850.0, 450.0]).with_decorations(true).with_transparent(true).with_app_id("temp_monitor_sobes");
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Teploměr", options, Box::new(|cc| Box::new(TempMonitorApp::new(cc))))
}

async fn bluetooth_scanner(tx: mpsc::Sender<AppMessage>, shared_config: Arc<Mutex<Config>>) {
    info!("Spouštím hlavní smyčku Bluetooth scanneru.");
    loop {
        let current_config = { if let Ok(config) = shared_config.lock() { config.clone() } else { Config::default() } };
        debug!("Nová iterace scanneru, MAC: {}", current_config.target_mac);
        let manager = match Manager::new().await {
            Ok(m) => m,
            Err(e) => {
                error!("Chyba při inicializaci BT manažeru: {}", e);
                let _ = tx.send(AppMessage::StatusUpdate("Chyba: BT adaptér nenalezen".into()));
                thread::sleep(Duration::from_secs(if current_config.continuous_mode { 1 } else { current_config.scan_pause_secs }));
                continue;
            }
        };
        if let Some(central) = manager.adapters().await.unwrap_or_default().into_iter().next() {
            let status_msg = if current_config.continuous_mode { "Skenuji (kontinuální režim)..." } else { "Skenuji..." };
            info!("Zahajuji skenování na adaptéru...");
            let _ = tx.send(AppMessage::StatusUpdate(status_msg.into()));
            if central.start_scan(ScanFilter::default()).await.is_ok() {
                let scan_duration = if current_config.continuous_mode { 60 } else { current_config.scan_timeout_secs };
                let _ = tokio::time::timeout(Duration::from_secs(scan_duration), async {
                    let mut events = central.events().await.unwrap();
                    while let Some(event) = events.next().await {
                        if let CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) = event {
                            if let Ok(p) = central.peripheral(&id).await {
                                if let Ok(Some(props)) = p.properties().await {
                                    if props.address.to_string().eq_ignore_ascii_case(&current_config.target_mac) {
                                        info!("Cílové zařízení nalezeno: {}", props.address);
                                        if let Some((company_id, data)) = props.manufacturer_data.iter().next() {
                                            if data.len() >= 2 {
                                                let temp = i16::from_le_bytes([(*company_id >> 8) as u8, data[0]]) as f32 / 10.0;
                                                let hum = data[1];
                                                let data_point = BleDataPoint { timestamp: Local::now(), temp, hum, device_id: id.to_string(), rssi: props.rssi, raw_data: data.clone() };
                                                info!("Úspěšně parsována data, posílám do procesoru: T={:.1}C, H={}%", temp, hum);
                                                if tx.send(AppMessage::NewData(data_point)).is_err() { break; }
                                                if !current_config.continuous_mode { return; }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }).await;
                info!("Skenování ukončeno (timeout).");
                let _ = central.stop_scan().await;
            }
        }
        let _ = tx.send(AppMessage::StatusUpdate("Čekám...".into()));
        let pause_duration = if current_config.continuous_mode { 1 } else { current_config.scan_pause_secs };
        debug!("Pauza na {} sekund.", pause_duration);
        thread::sleep(Duration::from_secs(pause_duration));
    }
}