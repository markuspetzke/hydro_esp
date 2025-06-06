use anyhow::Result;
use chrono::NaiveTime;
use embedded_svc::http::client::Client;
use embedded_svc::io::Write;
use embedded_svc::wifi::{ClientConfiguration, Configuration};
use esp_idf_hal::adc::attenuation::DB_11;
use esp_idf_hal::adc::oneshot::config::AdcChannelConfig;
use esp_idf_hal::adc::oneshot::AdcDriver;
use esp_idf_hal::adc::oneshot::*;
use esp_idf_hal::gpio::PinDriver;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};
use esp_idf_svc::sntp::{EspSntp, SntpConf, SyncStatus};
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition, wifi::EspWifi};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::{thread, thread::sleep, time::Duration};
const PH_SLOPE: f32 = -5.7;
const ADC_REF_VOLTAGE: f32 = 3.3;
const CALIBRATION: f32 = 21.00;
const SERVER: &str = env!("SERVER");

#[derive(Debug, Deserialize, Clone)]
struct Settings {
    day_pump: u64,
    day_break: u64,
    night_pump: u64,
    night_break: u64,
    mess_interval: u64,

    night_start: NaiveTime,
    day_start: NaiveTime,
}

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();

    let peripherals = Peripherals::take().unwrap();
    let modem = peripherals.modem;
    let adc1 = peripherals.adc1;
    let gpio34 = peripherals.pins.gpio34;
    let gpio27 = peripherals.pins.gpio27;

    //Wifi setup
    let mut wifi_driver = EspWifi::new(
        modem,
        EspSystemEventLoop::take().unwrap(),
        Some(EspDefaultNvsPartition::take().unwrap()),
    )
    .unwrap();

    let ap_config = Configuration::Client(ClientConfiguration {
        ssid: env!("SSID").try_into().unwrap(),
        password: env!("SSID_PASSWORD").try_into().unwrap(),
        ..Default::default()
    });

    wifi_driver.set_configuration(&ap_config).unwrap();
    wifi_driver.start().unwrap();
    wifi_driver.connect().unwrap();

    while !wifi_driver.is_connected().unwrap() {
        wifi_driver.get_configuration().unwrap();
        sleep(Duration::new(10, 0));
    }

    //NTP sync
    std::env::set_var("TZ", "CET-1CEST,M3.5.0/2,M10.5.0/3");
    let sntp = EspSntp::new(&SntpConf {
        servers: ["europe.pool.ntp.org"],
        ..Default::default()
    })?;
    while sntp.get_sync_status() != SyncStatus::Completed {
        thread::sleep(Duration::new(2, 0));
    }
    println!("Time Sync Completed");

    let settings: Arc<Mutex<Option<Settings>>> = Arc::new(Mutex::new(None));
    let settings_clone = Arc::clone(&settings);

    let led = Arc::new(Mutex::new(PinDriver::output(gpio27)?));
    let led_clone = Arc::clone(&led);
    //control pump
    let _ = thread::Builder::new()
        .stack_size(12 * 1024)
        .spawn(move || loop {
            if let Ok(locked) = settings_clone.lock() {
                if let Some(val) = (*locked).clone() {
                    drop(locked);
                    let now = chrono::Local::now().time();

                    let mut led = led_clone.lock().unwrap();
                    if val.day_start <= now && now < val.night_start {
                        println!("Tagbetrieb");

                        println!("Pumpe an");
                        led.set_high().unwrap();
                        thread::sleep(Duration::from_secs(val.day_pump));

                        println!("Pumpe aus");
                        led.set_low().unwrap();
                        thread::sleep(Duration::from_secs(val.day_break));
                    } else {
                        println!("Nachtbetrieb");

                        println!("Pumpe an");
                        led.set_high().unwrap();
                        thread::sleep(Duration::from_secs(val.night_pump));

                        println!("Pumpe aus");
                        led.set_low().unwrap();
                        thread::sleep(Duration::from_secs(val.night_break));
                    }
                } else {
                    println!("Settings sind noch nicht gesetzt");
                    thread::sleep(Duration::from_secs(30));
                }
            } else {
                eprintln!("Fehler beim Locken des Mutex");
                thread::sleep(Duration::from_secs(30));
            }
        }); // //Fetch settings
    let settings_clone = Arc::clone(&settings);
    let _ = thread::Builder::new()
        .stack_size(4 * 1024)
        .spawn(move || loop {
            match EspHttpConnection::new(&HttpConfig::default()) {
                Ok(httpconnection) => {
                    let headers = &[("Content-Type", "application/json")];
                    let mut httpclient = Client::wrap(httpconnection);
                    let conc = format!("{SERVER}get_settings.php");

                    match httpclient.post(conc.as_str(), headers) {
                        Ok(request) => {
                            match request.submit() {
                                Ok(mut response) => {
                                    let mut body = vec![0u8; 512];
                                    match response.read(&mut body) {
                                        Ok(size) => {
                                            let content =
                                                String::from_utf8_lossy(&body[..size]).to_string();
                                            match settings_clone.lock() {
                                                Ok(mut locked) => {
                                                    match serde_json::from_str::<Settings>(&content)
                                                    {
                                                        Ok(parses) => *locked = Some(parses),
                                                        Err(_) => {
                                                            eprintln!(
                                                                "Error beim Parsen der Settings"
                                                            )
                                                        }
                                                    }
                                                    println!("Antwort vom Server: {}", content);
                                                }
                                                Err(poisoned) => {
                                                    eprintln!("Mutex poisoned: {:?}", poisoned);
                                                    // Recovery-Möglichkeit hier (z.B. Mutex neu initialisieren)
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("Fehler beim Lesen der Antwort: {:?}", e)
                                        }
                                    }
                                }
                                Err(e) => eprintln!("Fehler beim Senden der Anfrage: {:?}", e),
                            }
                        }
                        Err(e) => eprintln!("Fehler beim Erstellen der POST-Anfrage: {:?}", e),
                    }
                }
                Err(e) => eprintln!("Fehler beim Aufbau der HTTP-Verbindung: {:?}", e),
            }

            thread::sleep(Duration::from_secs(60 * 10));
        });

    let settings_clone = Arc::clone(&settings);

    thread::Builder::new()
        .stack_size(6 * 1024)
        .spawn(move || {
            let config = AdcChannelConfig {
                attenuation: DB_11,
                ..Default::default()
            };
            let adc = AdcDriver::new(adc1).unwrap();
            let mut adc_pin = AdcChannelDriver::new(&adc, gpio34, &config).unwrap();

            loop {
                // 1. ADC-Werte sammeln
                let mut samples = [0u16; 10];
                for val in &mut samples {
                    match adc.read(&mut adc_pin) {
                        Ok(v) => *val = v,
                        Err(e) => {
                            eprintln!("ADC Lesefehler: {:?}", e);
                            *val = 0;
                        }
                    }
                    sleep(Duration::from_millis(30));
                }

                samples.sort_unstable();
                let avg: u32 = samples[2..8].iter().map(|&v| v as u32).sum::<u32>() / 6;

                // 2. Spannung berechnen
                let corrected = avg as f32 / 0.597;
                let voltage: f32 = corrected * ADC_REF_VOLTAGE / 4095.0;

                // 3. pH berechnen
                let ph = CALIBRATION + PH_SLOPE * voltage;
                println!("Gemessener pH-Wert: {:.3}", ph);

                // 4. pH-Wert validieren und senden
                if (2.0..=14.0).contains(&ph) {
                    if let Ok(httpconnection) = EspHttpConnection::new(&HttpConfig::default()) {
                        let body = json!({
                            "ph_value": format!("{:.3}", ph),
                            "sensor_id": env!("ID")
                        })
                        .to_string();
                        let headers = &[
                            ("Content-Type", "application/json"),
                            ("Content-Length", &body.len().to_string()),
                        ];

                        let mut httpclient = Client::wrap(httpconnection);
                        if let Ok(mut request) =
                            httpclient.post(&format!("{SERVER}add_ph.php"), headers)
                        {
                            if let Err(e) = request.write_all(body.as_bytes()) {
                                eprintln!("Fehler beim Schreiben der Anfrage: {:?}", e);
                            }
                            if let Err(e) = request.submit() {
                                eprintln!("Fehler beim Senden der Anfrage: {:?}", e);
                            }
                        } else {
                            eprintln!("Fehler beim Erstellen der HTTP POST-Anfrage");
                        }
                    } else {
                        eprintln!("Fehler beim Aufbau der HTTP-Verbindung");
                    }
                } else {
                    eprintln!("Ungültiger pH-Wert: {:.3}", ph);
                }

                // 5. Schlafdauer aus Settings lesen
                let sleep_duration = if let Ok(lock) = settings_clone.lock() {
                    if let Some(ref settings) = *lock {
                        Duration::from_secs(settings.mess_interval)
                    } else {
                        Duration::from_secs(30)
                    }
                } else {
                    Duration::from_secs(30)
                };

                thread::sleep(sleep_duration);
            }
        })
        .unwrap();
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}
