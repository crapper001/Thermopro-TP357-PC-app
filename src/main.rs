// Atribut pro skrytí konzolového okna ve finální verzi (v release buildu)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// --- Importy ---
use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral, ScanFilter};
use btleplug::platform::Manager;
use chrono::{DateTime, Local, NaiveDateTime};
use eframe::egui;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// --- Konstanty a Konfigurace ---
const MAX_HISTORY_POINTS: usize = 200;
const CONFIG_FILE: &str = "config.json";

// --- DATOVÉ STRUKTURY ---

#[derive(Serialize, Deserialize, Clone)]
struct Config {
    target_mac: String,
    scan_timeout_secs: u64,
    scan_pause_secs: u64,
    temp_warn_high: f32,
    temp_warn_low: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            target_mac: "B8:59:CE:33:0F:93".to_string(),
            scan_timeout_secs: 15,
            scan_pause_secs: 10,
            temp_warn_high: 30.0,
            temp_warn_low: 10.0,
        }
    }
}

#[derive(Clone)]
struct HistoryPoint {
    timestamp: DateTime<Local>,
    temp: f32,
    hum: u8,
}

struct BleDataPoint {
    timestamp: DateTime<Local>,
    temp: f32,
    hum: u8,
    device_id: String,
    rssi: Option<i16>,
    raw_data: Vec<u8>,
}

enum ScannerMessage {
    NewData(BleDataPoint),
    StatusUpdate(String),
}

// --- Datová struktura aplikace ---
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct TempMonitorApp {
    config: Config,
    settings_open: bool,

    #[serde(skip)]
    rx: mpsc::Receiver<ScannerMessage>,
    #[serde(skip)]
    history: VecDeque<HistoryPoint>,
    #[serde(skip)]
    last_data_point: Option<BleDataPoint>,
    #[serde(skip)]
    last_csv_write_ok: bool,
    #[serde(skip)]
    scan_status: String,
    #[serde(skip)]
    zoom_factor: f32,
    #[serde(skip)]
    reset_plot: bool,
}

impl Default for TempMonitorApp {
    fn default() -> Self {
        let (_tx, rx) = mpsc::channel();
        Self {
            config: load_config(),
            settings_open: false,
            rx,
            history: VecDeque::new(),
            last_data_point: None,
            last_csv_write_ok: true,
            scan_status: "Inicializace...".to_string(),
            zoom_factor: 1.0,
            reset_plot: false,
        }
    }
}

impl TempMonitorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app: Self = if let Some(storage) = cc.storage {
            eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default()
        } else {
            Default::default()
        };

        let (tx, rx) = mpsc::channel();
        app.rx = rx;

        let config_clone = app.config.clone();
        let rt = tokio::runtime::Runtime::new().expect("Nelze vytvořit Tokio runtime");
        rt.spawn(bluetooth_scanner(tx, config_clone));
        std::mem::forget(rt);

        app.history = load_history_from_csv();
        app
    }

    fn add_data_point(&mut self, data: BleDataPoint) {
        if self.history.len() >= MAX_HISTORY_POINTS {
            self.history.pop_front();
        }
        self.last_csv_write_ok = log_to_csv(data.temp, data.hum).is_ok();
        let history_point = HistoryPoint {
            timestamp: data.timestamp,
            temp: data.temp,
            hum: data.hum,
        };
        self.history.push_back(history_point);
        self.last_data_point = Some(data);
    }
}

// --- Logika GUI ---
impl eframe::App for TempMonitorApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
        save_config(&self.config);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                ScannerMessage::NewData(data_point) => self.add_data_point(data_point),
                ScannerMessage::StatusUpdate(status) => self.scan_status = status,
            }
        }

        let mut visual = egui::Visuals::dark();
        visual.window_fill = egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240);
        ctx.set_visuals(visual);

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Soubor", |ui| {
                    if ui.button("Nastavení").clicked() {
                        self.settings_open = true;
                        ui.close_menu();
                    }
                    if ui.button("Ukončit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.separator();
                // --- ZAČÁTEK OPRAVY: Přesně podle vašeho zadání ---
                if ui.button("➖").on_hover_text("Oddálit").clicked() {
                    self.zoom_factor = 0.7;
                }
                if ui.button("➕").on_hover_text("Přiblížit").clicked() {
                    self.zoom_factor = 1.25;
                }
                // --- KONEC OPRAVY ---
                if ui.button("⛶").on_hover_text("Vycentrovat graf").clicked() {
                    self.reset_plot = true;
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(4, |columns| {
                columns[0].vertical_centered(|ui| {
                    draw_temperature_info(ui, &self.history, &self.config);
                });
                columns[1].vertical_centered(|ui| {
                    draw_humidity_info(ui, &self.history);
                });
                columns[2].vertical(|ui| {
                    draw_scan_metadata(ui, &self.last_data_point, &self.scan_status);
                });
                columns[3].vertical(|ui| {
                    draw_data_details(ui, &self.last_data_point, self.last_csv_write_ok);
                });
            });
            ui.separator();
            draw_graph(self, ui, ctx);
        });

        self.draw_settings_window(ctx);
    }
}

impl TempMonitorApp {
    fn draw_settings_window(&mut self, ctx: &egui::Context) {
        if self.settings_open {
            let mut is_open = self.settings_open;
            egui::Window::new("Nastavení").open(&mut is_open).show(ctx, |ui| {
                ui.label("Cílová MAC adresa:");
                ui.text_edit_singleline(&mut self.config.target_mac);
                ui.separator();
                ui.add(egui::DragValue::new(&mut self.config.scan_timeout_secs).prefix("Timeout skenování (s): "));
                ui.add(egui::DragValue::new(&mut self.config.scan_pause_secs).prefix("Pauza mezi skeny (s): "));
                ui.separator();
                ui.add(egui::DragValue::new(&mut self.config.temp_warn_high).prefix("Mez pro varování (°C): ").speed(0.1));
                ui.add(egui::DragValue::new(&mut self.config.temp_warn_low).prefix("Spodní mez (°C): ").speed(0.1));
            });
            self.settings_open = is_open;
        }
    }
}

// --- Vykreslovací, I/O a logovací funkce ---

fn get_daily_log_filename() -> String {
    Local::now().format("log_%Y-%m-%d.csv").to_string()
}

fn draw_temperature_info(ui: &mut egui::Ui, history: &VecDeque<HistoryPoint>, config: &Config) {
    let temp_min = history.iter().map(|p| p.temp).min_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal)).unwrap_or(0.0);
    let temp_max = history.iter().map(|p| p.temp).max_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal)).unwrap_or(0.0);
    
    ui.label(egui::RichText::new("Teplota").color(egui::Color32::GRAY));
    if let Some(point) = history.back() {
        let current_temp = point.temp;
        let mut color = egui::Color32::from_rgb(255, 100, 100);
        if current_temp > config.temp_warn_high {
            color = egui::Color32::GOLD;
        } else if current_temp < config.temp_warn_low {
            color = egui::Color32::from_rgb(120, 180, 255);
        }
        ui.label(egui::RichText::new(format!("{:.1}°C", current_temp)).size(32.0).color(color));
    } else {
        ui.label(egui::RichText::new("N/A").size(32.0));
    }
    ui.label(format!("Min: {:.1}° / Max: {:.1}°", temp_min, temp_max));
}

fn draw_humidity_info(ui: &mut egui::Ui, history: &VecDeque<HistoryPoint>) {
    let hum_min = history.iter().map(|p| p.hum).min().unwrap_or(0);
    let hum_max = history.iter().map(|p| p.hum).max().unwrap_or(0);

    ui.label(egui::RichText::new("Vlhkost").color(egui::Color32::GRAY));
    if let Some(point) = history.back() {
        ui.label(egui::RichText::new(format!("{}%", point.hum)).size(32.0).color(egui::Color32::from_rgb(100, 100, 255)));
    } else {
        ui.label(egui::RichText::new("N/A").size(32.0));
    }
    ui.label(format!("Min: {}% / Max: {}%", hum_min, hum_max));
}

fn draw_scan_metadata(ui: &mut egui::Ui, last_data: &Option<BleDataPoint>, status: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Stav:").color(egui::Color32::GRAY));
        ui.label(status);
    });
    if let Some(data) = last_data {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Aktualizace:").color(egui::Color32::GRAY));
            ui.label(data.timestamp.format("%H:%M:%S").to_string());
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("RSSI:").color(egui::Color32::GRAY));
            if let Some(rssi) = data.rssi {
                ui.label(format!("{} dBm", rssi));
            } else {
                ui.label("N/A");
            }
        });
    }
}

fn draw_data_details(ui: &mut egui::Ui, last_data: &Option<BleDataPoint>, csv_ok: bool) {
    if let Some(data) = last_data {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("ID Zařízení:").color(egui::Color32::GRAY));
            ui.label(data.device_id.to_string());
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Raw data:").color(egui::Color32::GRAY));
            ui.label(data.raw_data.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Zápis CSV:").color(egui::Color32::GRAY));
            if csv_ok {
                ui.label(egui::RichText::new("OK").color(egui::Color32::GREEN));
            } else {
                ui.label(egui::RichText::new("Chyba").color(egui::Color32::RED));
            }
        });
    }
}

fn draw_graph(app: &mut TempMonitorApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    use egui_plot::{AxisHints, GridMark, Line, Plot, PlotPoints};
    
    let humidity_scale = 0.55;
    let humidity_offset = -10.0;

    let temp_points = app.history.iter()
        .map(|p| [p.timestamp.timestamp() as f64, p.temp as f64])
        .collect::<PlotPoints>();
    
    let hum_points = app.history.iter()
        .map(|p| [p.timestamp.timestamp() as f64, p.hum as f64 * humidity_scale + humidity_offset])
        .collect::<PlotPoints>();

    let temp_line = Line::new(temp_points).color(egui::Color32::from_rgb(255, 100, 100)).name("Teplota");
    let hum_line = Line::new(hum_points).color(egui::Color32::from_rgb(100, 100, 255)).name("Vlhkost");

    let mut plot = Plot::new("history_plot")
        .height(ui.available_height() - 10.0)
        .show_background(false)
        .allow_drag(true)
        .allow_zoom(true)
        .x_axis_formatter(|mark: GridMark, _, _| {
            let time = DateTime::from_timestamp(mark.value as i64, 0).unwrap_or_default().with_timezone(&Local);
            time.format("%H:%M:%S").to_string()
        })
        .legend(egui_plot::Legend::default())
        .custom_y_axes(vec![
            AxisHints::new_y()
                .label("Teplota (°C)")
                .placement(egui_plot::HPlacement::Left)
                .formatter(Box::new(|mark: GridMark, _digits: usize, _range: &std::ops::RangeInclusive<f64>| -> String {
                    format!("{:.0}°C", mark.value)
                })),
            AxisHints::new_y()
                .label("Vlhkost (%)")
                .placement(egui_plot::HPlacement::Right)
                .formatter(Box::new(move |mark: GridMark, _digits: usize, _range: &std::ops::RangeInclusive<f64>| -> String {
                    let humidity_percent: f64 = (mark.value - humidity_offset) / humidity_scale;
                    format!("{:.0}%", humidity_percent.max(0.0).min(100.0))
                })),
        ]);

    if app.reset_plot {
        plot = plot.reset();
    }

    plot.show(ui, |plot_ui| {
        plot_ui.line(temp_line);
        plot_ui.line(hum_line);

        if let Some(pointer_pos) = plot_ui.pointer_coordinate() {
            if let Some(closest_point) = app.history.iter().min_by(|a, b| {
                let diff_a = (a.timestamp.timestamp() as f64 - pointer_pos.x).abs();
                let diff_b = (b.timestamp.timestamp() as f64 - pointer_pos.x).abs();
                diff_a.partial_cmp(&diff_b).unwrap_or(Ordering::Equal)
            }) {
                let plot_width_secs = plot_ui.plot_bounds().width();
                if (closest_point.timestamp.timestamp() as f64 - pointer_pos.x).abs() < plot_width_secs * 0.01 {
                    let temp_y = closest_point.temp as f64;
                    let hum_y_scaled = closest_point.hum as f64 * humidity_scale + humidity_offset;
                    let dist_to_temp = (pointer_pos.y - temp_y).abs();
                    let dist_to_hum = (pointer_pos.y - hum_y_scaled).abs();
                    
                    let formatted_time = closest_point.timestamp.format("%H:%M:%S").to_string();

                    let screen_pos = plot_ui.screen_from_plot(pointer_pos);
                    egui::Area::new("plot_tooltip_area".into())
                        .fixed_pos(screen_pos + egui::vec2(10.0, 10.0))
                        .order(egui::Order::Tooltip)
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                if dist_to_temp < dist_to_hum {
                                    ui.label(format!("Teplota\nČas: {}\nHodnota: {:.1}°C", formatted_time, closest_point.temp));
                                } else {
                                    ui.label(format!("Vlhkost\nČas: {}\nHodnota: {}%", formatted_time, closest_point.hum));
                                }
                            });
                        });
                }
            }
        }

        if app.zoom_factor != 1.0 {
            plot_ui.zoom_bounds(
                egui::vec2(app.zoom_factor, app.zoom_factor), 
                plot_ui.plot_bounds().center()
            );
            app.zoom_factor = 1.0;
        }
    });

    if app.reset_plot {
        app.reset_plot = false;
    }
}

fn log_to_csv(temp: f32, hum: u8) -> Result<(), csv::Error> {
    let filename = get_daily_log_filename();
    let file_exists = Path::new(&filename).exists();
    let file = OpenOptions::new().append(true).create(true).open(filename)?;

    let mut wtr = csv::WriterBuilder::new().delimiter(b';').from_writer(file);
    if !file_exists {
        wtr.write_record(&["Datum", "Cas", "Teplota", "Vlhkost"])?;
    }
    let now = Local::now();
    let temp_str = format!("{:.1}", temp).replace('.', ",");
    wtr.write_record(&[
        now.format("%Y.%m.%d").to_string(),
        now.format("%H:%M:%S").to_string(),
        temp_str,
        hum.to_string(),
    ])?;
    wtr.flush()?;
    Ok(())
}

fn load_history_from_csv() -> VecDeque<HistoryPoint> {
    let mut history = VecDeque::with_capacity(MAX_HISTORY_POINTS);
    let filename = get_daily_log_filename();
    if let Ok(file) = File::open(filename) {
        let mut rdr = csv::ReaderBuilder::new().delimiter(b';').from_reader(file);
        let all_records: Vec<_> = rdr.records().filter_map(Result::ok).collect();
        let start_index = all_records.len().saturating_sub(MAX_HISTORY_POINTS);

        for result in all_records.iter().skip(start_index) {
            if let (Some(date_str), Some(time_str), Some(temp_str), Some(hum_str)) = (result.get(0), result.get(1), result.get(2), result.get(3)) {
                let datetime_str = format!("{} {}", date_str, time_str);
                if let Ok(naive_dt) = NaiveDateTime::parse_from_str(&datetime_str, "%Y.%m.%d %H:%M:%S") {
                    if let (Ok(temp), Ok(hum)) = (temp_str.replace(',', ".").parse(), hum_str.parse()) {
                        history.push_back(HistoryPoint {
                            timestamp: naive_dt.and_local_timezone(Local).unwrap(),
                            temp,
                            hum,
                        });
                    }
                }
            }
        }
    }
    history
}

fn load_config() -> Config {
    fs::read_to_string(CONFIG_FILE).ok().and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default()
}

fn save_config(config: &Config) {
    if let Ok(content) = serde_json::to_string_pretty(config) {
        let _ = fs::write(CONFIG_FILE, content);
    }
}

// --- Spouštěcí logika a Bluetooth vlákno ---
fn main() -> Result<(), eframe::Error> {
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([850.0, 450.0])
        .with_decorations(true)
        .with_transparent(true)
        .with_app_id("temp_monitor_sobes");

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("Teploměr", options, Box::new(|cc| Box::new(TempMonitorApp::new(cc))))
}

async fn bluetooth_scanner(tx: mpsc::Sender<ScannerMessage>, config: Config) {
    loop {
        let manager_result = Manager::new().await;
        let manager = match manager_result {
            Ok(m) => m,
            Err(_) => {
                let _ = tx.send(ScannerMessage::StatusUpdate("Chyba: BT adaptér nenalezen".into()));
                thread::sleep(Duration::from_secs(config.scan_pause_secs));
                continue;
            }
        };

        if let Some(central) = manager.adapters().await.unwrap_or_default().into_iter().next() {
            let _ = tx.send(ScannerMessage::StatusUpdate("Skenuji...".into()));
            if central.start_scan(ScanFilter::default()).await.is_ok() {
                let _ = tokio::time::timeout(Duration::from_secs(config.scan_timeout_secs), async {
                    let mut events = central.events().await.unwrap();
                    while let Some(event) = events.next().await {
                        if let CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) = event {
                            if let Ok(p) = central.peripheral(&id).await {
                                if let Ok(Some(props)) = p.properties().await {
                                    if props.address.to_string().eq_ignore_ascii_case(&config.target_mac) {
                                        if let Some((company_id, data)) = props.manufacturer_data.iter().next() {
                                            if data.len() >= 2 {
                                                let temp = i16::from_le_bytes([(*company_id >> 8) as u8, data[0]]) as f32 / 10.0;
                                                let hum = data[1];
                                                let data_point = BleDataPoint {
                                                    timestamp: Local::now(), temp, hum,
                                                    device_id: id.to_string(), rssi: props.rssi, raw_data: data.clone(),
                                                };
                                                if tx.send(ScannerMessage::NewData(data_point)).is_ok() { return; }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }).await;
                let _ = central.stop_scan().await;
            }
        }
        let _ = tx.send(ScannerMessage::StatusUpdate("Čekám...".into()));
        thread::sleep(Duration::from_secs(config.scan_pause_secs));
    }
}