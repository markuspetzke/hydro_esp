use anyhow::Result;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::{eventloop::EspSystemEventLoop, nvs::EspDefaultNvsPartition, wifi::EspWifi};
use std::{thread::sleep, time::Duration};

use embedded_svc::http::client::Client;
use embedded_svc::io::Write;
use embedded_svc::wifi::{ClientConfiguration, Configuration};
use esp_idf_svc::hal::adc::{AdcContConfig, AdcContDriver, AdcMeasurement, Attenuated};
use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("SSID_PASSWORD");
const CALIBRATION: f32 = 20.70;

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    let Peripherals {
        pins,
        modem,
        adc1,
        i2s0,
        ..
    } = Peripherals::take().unwrap();

    let sys_loop = EspSystemEventLoop::take().unwrap();
    let nvs = EspDefaultNvsPartition::take().unwrap();

    let mut wifi_driver = EspWifi::new(modem, sys_loop, Some(nvs)).unwrap();

    let ap_config = Configuration::Client(ClientConfiguration {
        ssid: SSID.try_into().unwrap(),
        password: PASSWORD.try_into().unwrap(),
        ..Default::default()
    });

    wifi_driver.set_configuration(&ap_config).unwrap();
    wifi_driver.start().unwrap();
    wifi_driver.connect().unwrap();

    while !wifi_driver.is_connected().unwrap() {
        wifi_driver.get_configuration().unwrap();
        sleep(Duration::new(10, 0));
    }

    let adc_pin = Attenuated::db11(pins.gpio34);
    let mut adc = AdcContDriver::new(adc1, i2s0, &AdcContConfig::default(), adc_pin)?;

    let url = env!("URL");
    adc.start()?;
    loop {
        let ph = {
            let mut samples = [AdcMeasurement::default(); 10];

            if let Ok(num_read) = adc.read(&mut samples, 10) {
                let mut raw_values: Vec<u16> =
                    samples[..num_read].iter().map(|m| m.data()).collect();

                raw_values.sort_unstable();
                let avg: u32 = raw_values[2..8].iter().map(|&v| v as u32).sum();
                let avg = avg as f32 / 6.0;
                let voltage = avg * 3.3 / 4095.0;
                -5.70 * voltage + CALIBRATION
            } else {
                0.0
            }
        };

        let httpconnection = EspHttpConnection::new(&HttpConfig::default())?;
        let body = format!("ph_value={}", ph);
        let headers = &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Content-Length", &body.len().to_string()),
        ];

        let mut httpclient = Client::wrap(httpconnection);
        let mut request = httpclient.post(url, headers)?;
        request.write_all(body.as_bytes())?;
        request.submit()?;

        sleep(Duration::new(10, 0));
    }
}
