/*
 * SPDX-License-Identifier: MIT
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 */
use serde::{Deserialize, Serialize};

use super::{ODataLinks, ResourceStatus, StatusVec};
use crate::model::sensor::Sensor;
use crate::model::ODataId;
use crate::network::{RedfishHttpClient, REDFISH_ENDPOINT};
use crate::RedfishError;

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct FansOemHp {
    #[serde(flatten)]
    pub fan_type: super::oem::hpe::HpType,
    pub location: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct FansOem {
    pub hp: FansOemHp,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct FanThresholdReading {
    reading: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct FanThresholds {
    pub lower_critical: FanThresholdReading,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Fan {
    pub reading: Option<f64>,
    pub reading_units: String,
    pub fan_name: Option<String>, // Dell, Lenovo, NVIDIA DPU
    pub name: Option<String>,     // Supermicro
    pub physical_context: Option<String>,
    pub sensor_number: Option<i64>,
    pub lower_threshold_critical: Option<i64>,
    pub lower_threshold_fatal: Option<i64>,
    pub status: ResourceStatus,
    pub upper_threshold_critical: Option<i64>,
    pub upper_threshold_fatal: Option<i64>,
    pub thresholds: Option<FanThresholds>,
}

/// One `TemperatureReadingsCelsius` entry on NVIDIA OpenBMC host BMCs.
/// GB200 includes `@odata.id`; Vera Rubin may omit it and only provide `DataSourceUri`.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct TemperatureOemNvidia {
    #[serde(rename = "@odata.id", default)]
    pub odata_id: Option<String>,
    pub data_source_uri: Option<String>,
    pub device_name: Option<String>,
    pub physical_context: Option<String>,
    pub reading: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct TemperaturesOemNvidia {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub id: String,
    pub name: String,
    pub temperature_readings_celsius: Option<Vec<TemperatureOemNvidia>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct TemperaturesOemHp {
    #[serde(flatten)]
    pub temp_type: super::oem::hpe::HpType,
    pub location_xmm: i64,
    pub location_ymm: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct TemperaturesOem {
    pub hp: TemperaturesOemHp,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Temperature {
    pub name: String,
    pub sensor_number: Option<i64>,
    pub lower_threshold_critical: Option<f64>,
    pub lower_threshold_fatal: Option<f64>,
    pub physical_context: Option<String>,
    pub reading_celsius: Option<f64>,
    pub status: ResourceStatus,
    pub upper_threshold_critical: Option<f64>,
    pub upper_threshold_fatal: Option<f64>,
}

impl Default for Temperature {
    fn default() -> Self {
        Self {
            name: "".to_string(),
            sensor_number: None,
            lower_threshold_critical: None,
            lower_threshold_fatal: None,
            physical_context: None,
            reading_celsius: None,
            status: Default::default(),
            upper_threshold_critical: None,
            upper_threshold_fatal: None,
        }
    }
}

impl From<TemperatureOemNvidia> for Temperature {
    fn from(temp: TemperatureOemNvidia) -> Self {
        let name = temp
            .device_name
            .or_else(|| sensor_name_from_uri(temp.data_source_uri.as_deref()))
            .or_else(|| sensor_name_from_uri(temp.odata_id.as_deref()))
            .unwrap_or_else(|| "Unknown".to_string());
        Self {
            name,
            reading_celsius: temp.reading,
            physical_context: temp.physical_context,
            ..Default::default()
        }
    }
}

fn sensor_name_from_uri(uri: Option<&str>) -> Option<String> {
    uri.and_then(|uri| uri.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

impl From<Sensor> for Temperature {
    fn from(sensor: Sensor) -> Self {
        let physical_context = sensor
            .physical_context
            .map(|physical_context| physical_context.to_string());
        Self {
            name: sensor.name.unwrap_or("".to_string()),
            sensor_number: None,
            lower_threshold_critical: None,
            lower_threshold_fatal: None,
            physical_context,
            reading_celsius: sensor.reading,
            status: sensor.status.unwrap_or_default(),
            upper_threshold_critical: None,
            upper_threshold_fatal: None,
        }
    }
}

/// A single entry of a `ThermalMetrics.TemperatureReadingsCelsius` array
/// (Redfish `SensorArrayExcerpt`).
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct TemperatureReading {
    pub device_name: Option<String>,
    pub reading: Option<f64>,
    pub physical_context: Option<String>,
}

impl From<TemperatureReading> for Temperature {
    fn from(reading: TemperatureReading) -> Self {
        Self {
            name: reading.device_name.unwrap_or_default(),
            reading_celsius: reading.reading,
            physical_context: reading.physical_context,
            ..Default::default()
        }
    }
}

/// The `ThermalMetrics` resource
/// (`/redfish/v1/Chassis/<id>/ThermalSubsystem/ThermalMetrics`), which carries
/// temperature readings as a `TemperatureReadingsCelsius` array.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ThermalMetrics {
    #[serde(flatten)]
    pub odata: Option<ODataLinks>,
    pub id: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub temperature_readings_celsius: Vec<TemperatureReading>,
}

/// The `ThermalSubsystem` resource
/// (`/redfish/v1/Chassis/<id>/ThermalSubsystem`), the newer Redfish
/// replacement for the legacy `Thermal` resource, which links to
/// `ThermalMetrics`.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ThermalSubsystem {
    #[serde(flatten)]
    pub odata: Option<ODataLinks>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub thermal_metrics: Option<ODataId>,
}

impl ThermalSubsystem {
    /// Fetch this subsystem's `ThermalMetrics` and map its
    /// `TemperatureReadingsCelsius` entries to [`Temperature`]s. Returns an
    /// empty vec when the subsystem advertises no `ThermalMetrics` link.
    pub(crate) async fn temperatures(
        &self,
        client: &RedfishHttpClient,
    ) -> Result<Vec<Temperature>, RedfishError> {
        let Some(link) = &self.thermal_metrics else {
            return Ok(Vec::new());
        };
        let url = link.odata_id.replace(&format!("/{REDFISH_ENDPOINT}/"), "");
        let (_, metrics): (_, ThermalMetrics) = client.get(&url).await?;
        Ok(metrics
            .temperature_readings_celsius
            .into_iter()
            .map(Temperature::from)
            .collect())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Redundancy {
    pub max_num_supported: Option<i64>,
    pub member_id: String,
    pub min_num_needed: Option<i64>,
    pub mode: String,
    pub name: String,
    pub redundancy_enabled: bool,
    pub status: ResourceStatus,
    pub redundancy_set: Vec<ODataId>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct LeakDetector {
    pub name: String,
    pub id: String,
    pub leak_detector_type: Option<String>,
    pub detector_state: Option<String>,
    pub status: ResourceStatus,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Thermal {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub id: String,
    pub name: String,
    pub fans: Vec<Fan>,
    pub temperatures: Vec<Temperature>,
    pub redundancy: Option<Vec<Redundancy>>,
    pub leak_detectors: Option<Vec<LeakDetector>>,
}

impl Default for Thermal {
    fn default() -> Self {
        Self {
            odata: Default::default(),
            id: "".to_string(),
            name: "".to_string(),
            fans: vec![],
            temperatures: vec![],
            redundancy: None,
            leak_detectors: None,
        }
    }
}

impl StatusVec for Thermal {
    fn get_vec(&self) -> Vec<ResourceStatus> {
        let mut v = Vec::with_capacity(self.fans.len() + self.temperatures.len());
        for res in &self.fans {
            v.push(res.status)
        }
        for res in &self.temperatures {
            v.push(res.status)
        }
        v
    }
}

#[cfg(test)]
mod test {
    use super::{Temperature, TemperatureOemNvidia, TemperaturesOemNvidia};

    #[test]
    fn test_temperature_oem_nvidia_vera_rubin_without_odata_id() {
        let body = include_str!("testdata/thermal-nvidia-vr-thermalmetrics.json");
        let metrics: TemperaturesOemNvidia = serde_json::from_str(body).unwrap();
        let readings = metrics.temperature_readings_celsius.unwrap();
        assert_eq!(readings.len(), 1);
        assert!(readings[0].odata_id.is_none());
        assert_eq!(
            readings[0].data_source_uri.as_deref(),
            Some("/redfish/v1/Chassis/BMC_0/Sensors/BMC_0_Temp_0")
        );

        let temp: Temperature = readings[0].clone().into();
        assert_eq!(temp.name, "BMC_0_Temp_0");
        assert_eq!(temp.reading_celsius, Some(56.062));
    }

    #[test]
    fn test_temperature_oem_nvidia_gb200_with_odata_id() {
        let reading: TemperatureOemNvidia = serde_json::from_str(
            r#"{
                "@odata.id": "/redfish/v1/Chassis/BMC_0/Sensors/BMC_0_DCSCM_Temp_0",
                "DataSourceUri": "/redfish/v1/Chassis/BMC_0/Sensors/BMC_0_DCSCM_Temp_0",
                "DeviceName": "BMC_0_DCSCM_Temp_0",
                "Reading": 33.875
            }"#,
        )
        .unwrap();
        let temp: Temperature = reading.into();
        assert_eq!(temp.name, "BMC_0_DCSCM_Temp_0");
        assert_eq!(temp.reading_celsius, Some(33.875));
    }

    #[test]
    fn test_thermal_parser() {
        // TODO: hpe test data is obsolete, needs to be updated from latest iLO BMC
        // with newer redfish schema
        // let test_data_hpe = include_str!("testdata/thermal-hpe.json");
        // let result_hpe: super::Thermal = serde_json::from_str(test_data_hpe).unwrap();
        // println!("result: {result_hpe:#?}");
        let test_data_dell = include_str!("testdata/thermal-dell.json");
        let result_dell: super::Thermal = serde_json::from_str(test_data_dell).unwrap();
        println!("result: {result_dell:#?}");
        let test_data_lenovo = include_str!("testdata/thermal-lenovo.json");
        let result_lenovo: super::Thermal = serde_json::from_str(test_data_lenovo).unwrap();
        println!("result: {result_lenovo:#?}");
    }
}
