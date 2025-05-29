use anyhow::Result;
use esp_idf_hal::adc::oneshot::AdcDriver;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition, wifi::EspWifi};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::{thread, thread::sleep, time::Duration};

use embedded_svc::http::client::Client;
use embedded_svc::io::Write;
use embedded_svc::wifi::{ClientConfiguration, Configuration};
use esp_idf_hal::adc::attenuation::DB_11;
use esp_idf_hal::adc::oneshot::config::AdcChannelConfig;
use esp_idf_hal::adc::oneshot::*;
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};
const PH_SLOPE: f32 = -5.7;
const ADC_REF_VOLTAGE: f32 = 3.3;
const CALIBRATION: f32 = 21.00;
const SERVER: &str = env!("SERVER");

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();

    let Peripherals {
        pins, modem, adc1, ..
    } = Peripherals::take().unwrap();

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

    let adc = AdcDriver::new(adc1)?;
    let config = AdcChannelConfig {
        attenuation: DB_11,
        ..Default::default()
    };
    let mut adc_pin = AdcChannelDriver::new(&adc, pins.gpio34, &config)?;

    let settings: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let settings_clone = Arc::clone(&settings);

    thread::spawn(move || loop {
        if let Ok(httpconnection) = EspHttpConnection::new(&HttpConfig::default()) {
            let headers = &[("Content-Type", "application/json")];

            let mut httpclient = Client::wrap(httpconnection);
            let conc = format!("{SERVER}get_settings.php");
            let url: &str = &conc;

            if let Ok(request) = httpclient.post(url, headers) {
                match request.submit() {
                    Ok(mut response) => {
                        let mut body = [0u8; 512];
                        if let Ok(size) = response.read(&mut body) {
                            let content = String::from_utf8_lossy(&body[..size]).to_string();

                            // Speicher aktualisieren
                            if let Ok(mut locked) = settings_clone.lock() {
                                *locked = Some(content.clone());
                                println!("Antwort vom Server: {}", content);
                            }
                        } else {
                            eprintln!("Fehler beim Lesen der Antwort");
                        }
                    }
                    Err(e) => {
                        eprintln!("Fehler beim Senden der Anfrage: {:?}", e);
                    }
                }
            } else {
                eprintln!("Fehler beim Erstellen der POST-Anfrage");
            }
        } else {
            eprintln!("Fehler beim Aufbau der HTTP-Verbindung");
        }

        thread::sleep(Duration::from_secs(3));
    });
    loop {
        let mut samples = [0u16; 10];
        for val in &mut samples {
            *val = adc.read(&mut adc_pin)?;
            sleep(Duration::from_millis(30));
        }
        samples.sort_unstable();
        let avg: u32 = samples[2..8].iter().map(|&v| v as u32).sum::<u32>() / 6;
        let corrected = avg as f32 / 0.597; // "simulierte Kalibrierung"
        let voltage: f32 = corrected * ADC_REF_VOLTAGE / 4095.0;

        if let Ok(locked) = settings.lock() {
            if let Some(val) = &*locked {
                println!("Gespeicherte Settings: {}", val);
            } else {
                println!("Noch keine Settings erhalten");
            }
        }

        let ph = CALIBRATION + PH_SLOPE * voltage;

        if (2.0..=14.0).contains(&ph) {
            let httpconnection = EspHttpConnection::new(&HttpConfig::default())?;
            let body =
                json!({    "ph_value": format!("{:.3}", ph), "sensor_id": env!("ID")}).to_string();
            let headers = &[
                ("Content-Type", "application/json"),
                ("Content-Length", &body.len().to_string()),
            ];

            let mut httpclient = Client::wrap(httpconnection);
            let conc = format!("{SERVER}add_ph.php");
            let url: &str = &conc;
            let mut request = httpclient.post(url, headers)?;
            request.write_all(body.as_bytes())?;
            request.submit()?;
        }

        sleep(Duration::new(30, 0));
    }
}
