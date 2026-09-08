/*
 * SPDX-FileCopyrightText: Copyright (c) 2025 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
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
use crate::{Chassis, EnabledDisabled, REDFISH_ENDPOINT};
use regex::Regex;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::sync::OnceLock;
use std::{collections::HashMap, path::Path, time::Duration};
use tokio::fs::File;

use crate::model::component_integrity::RegexToFirmwareIdOptions;
use crate::model::sensor::{GPUSensors, Sensor, Sensors};
use crate::model::service_root::RedfishVendor;
use crate::model::storage::DriveCollection;
use crate::model::task::Task;
use crate::model::thermal::Fan;
use crate::model::update_service::ComponentType;
use crate::{
    jsonmap,
    model::{
        boot::{BootOverride, BootSourceOverrideEnabled, BootSourceOverrideTarget},
        power::{Power, PowerSupply, Voltages},
        sel::{LogEntry, LogEntryCollection},
        thermal::{LeakDetector, Temperature, TemperaturesOemNvidia, Thermal},
        BootOption, ComputerSystem,
    },
    standard::RedfishStandard,
    BiosProfileType, NetworkDeviceFunction, ODataId, Redfish, RedfishError,
};
use crate::{MachineSetupDiff, MachineSetupStatus};

const UEFI_PASSWORD_NAME: &str = "AdminPassword";

pub struct Bmc {
    s: RedfishStandard,
}

impl Bmc {
    pub fn new(s: RedfishStandard) -> Result<Bmc, RedfishError> {
        Ok(Bmc { s })
    }

    async fn is_supermicro_gb300(&self) -> Result<bool, RedfishError> {
        let systems = self
            .s
            .get_collection(ODataId::from("/redfish/v1/Systems"))
            .await?
            .try_get::<ComputerSystem>()?;
        Ok(systems_are_supermicro_gb300(&systems.members))
    }
}

fn systems_are_supermicro_gb300(systems: &[ComputerSystem]) -> bool {
    let is_supermicro = systems.iter().any(|system| {
        system
            .manufacturer
            .as_deref()
            .is_some_and(|manufacturer| manufacturer.eq_ignore_ascii_case("supermicro"))
    });
    let is_gb300 = systems.iter().any(|system| {
        system
            .model
            .as_deref()
            .is_some_and(|model| model.contains("GB300"))
    });

    is_supermicro && is_gb300
}

#[derive(Copy, Clone)]
pub enum BootOptionName {
    Http,
    Pxe,
    Hdd,
}

impl BootOptionName {
    fn to_string(self) -> &'static str {
        match self {
            BootOptionName::Http => "UEFI HTTPv4",
            BootOptionName::Pxe => "UEFI PXEv4",
            BootOptionName::Hdd => "HD(",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone, Eq, PartialEq)]
enum BootOptionMatchField {
    DisplayName,
    UefiDevicePath,
}

impl BootOptionMatchField {
    #[allow(dead_code)]
    fn to_string(self) -> &'static str {
        match self {
            BootOptionMatchField::DisplayName => "Display Name",
            BootOptionMatchField::UefiDevicePath => "Uefi Device Path",
        }
    }
}

impl Display for BootOptionMatchField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self, f)
    }
}

// Supported component to firmware mapping.
// GPU, Source: HGX_IRoT_GPU_X Target: HGX_FW_GPU_X
fn get_component_integrity_id_to_firmware_inventory_id_options(
) -> Result<&'static Vec<RegexToFirmwareIdOptions>, RedfishError> {
    static RE: OnceLock<Result<Vec<RegexToFirmwareIdOptions>, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Ok(vec![RegexToFirmwareIdOptions {
            id_prefix: "HGX_FW_",
            // Assuming our static pattern is good, this is probably
            // safe, but still check for an error instead of unwrapping.
            pattern: Regex::new(r"HGX_IRoT_(GPU_\d+)").map_err(|e| e.to_string())?,
        }])
    })
    .as_ref()
    .map_err(|e| RedfishError::GenericError {
        error: format!("Failed to compile regex: {}", e),
    })
}
impl Redfish for Bmc {
    fn std_redfish(&self) -> &RedfishStandard {
        &self.s
    }
    fn get_firmware<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::software_inventory::SoftwareInventory, RedfishError>,
    > {
        Box::pin(async move {
            let mut inv = self.s.get_firmware(id).await?;
            // BMC firmware gets prepended with "GB200Nvl-", (L, not 1!) so trim that off when we see it.
            inv.version = inv.version.map(|x| {
                x.strip_prefix("GB200Nvl-")
                    .unwrap_or(x.as_str())
                    .to_string()
            });
            Ok(inv)
        })
    }

    fn get_power_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Power, RedfishError>> {
        Box::pin(async move {
            let mut voltages = Vec::new();
            let mut power_supplies = Vec::new();
            // gb200 bianca has empty PowerSupplies on several chassis items
            // for now assemble power supply details from PDB_0 chassis entries
            let mut url = "Chassis/PDB_0".to_string();
            let (_status_code, pdb): (StatusCode, PowerSupply) = self.s.client.get(&url).await?;
            let mut hsc0 = pdb.clone();
            let mut hsc1 = pdb.clone();
            // voltage sensors are on several chassis items under sensors
            let chassis_all = self.s.get_chassis_all().await?;
            for chassis_id in chassis_all {
                url = format!("Chassis/{}", chassis_id);
                let (_status_code, chassis): (StatusCode, Chassis) =
                    self.s.client.get(&url).await?;
                if chassis.sensors.is_none() {
                    continue;
                }
                // walk through all Chassis/*/Sensors/ for voltage and PDB_0 for power supply details
                url = format!("Chassis/{}/Sensors", chassis_id);
                let (_status_code, sensors): (StatusCode, Sensors) =
                    self.s.client.get(&url).await?;
                for sensor in sensors.members {
                    if chassis_id == *"PDB_0" {
                        // get amps and watts for power supply
                        if sensor.odata_id.contains("HSC_0_Pwr") {
                            url = sensor
                                .odata_id
                                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                            let (_status_code, t): (StatusCode, Sensor) =
                                self.s.client.get(&url).await?;
                            hsc0.last_power_output_watts = t.reading;
                            hsc0.power_output_watts = t.reading;
                            hsc0.power_capacity_watts = t.reading_range_max;
                        }
                        if sensor.odata_id.contains("HSC_0_Cur") {
                            url = sensor
                                .odata_id
                                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                            let (_status_code, t): (StatusCode, Sensor) =
                                self.s.client.get(&url).await?;
                            hsc0.power_output_amps = t.reading;
                        }
                        if sensor.odata_id.contains("HSC_1_Pwr") {
                            url = sensor
                                .odata_id
                                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                            let (_status_code, t): (StatusCode, Sensor) =
                                self.s.client.get(&url).await?;
                            hsc1.last_power_output_watts = t.reading;
                            hsc1.power_output_watts = t.reading;
                            hsc1.power_capacity_watts = t.reading_range_max;
                        }
                        if sensor.odata_id.contains("HSC_1_Cur") {
                            url = sensor
                                .odata_id
                                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                            let (_status_code, t): (StatusCode, Sensor) =
                                self.s.client.get(&url).await?;
                            hsc1.power_output_amps = t.reading;
                        }
                    }
                    // now all voltage sensors in all chassis
                    if !sensor.odata_id.contains("Volt") {
                        continue;
                    }
                    url = sensor
                        .odata_id
                        .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                    let (_status_code, t): (StatusCode, Sensor) = self.s.client.get(&url).await?;
                    let sensor: Voltages = Voltages::from(t);
                    voltages.push(sensor);
                }
            }

            power_supplies.push(hsc0);
            power_supplies.push(hsc1);
            let power = Power {
                odata: None,
                id: "Power".to_string(),
                name: "Power".to_string(),
                power_control: vec![],
                power_supplies: Some(power_supplies),
                voltages: Some(voltages),
                redundancy: None,
            };
            Ok(power)
        })
    }

    fn power<'a>(
        &'a self,
        action: crate::SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if action == crate::SystemPowerControl::ACPowercycle {
                let args: HashMap<String, String> =
                    HashMap::from([("ResetType".to_string(), "AuxPowerCycle".to_string())]);
                return self
                    .s
                    .client
                    .post(
                        "Chassis/BMC_0/Actions/Oem/NvidiaChassis.AuxPowerReset",
                        args,
                    )
                    .await
                    .map(|_status_code| ());
            }

            self.s.power(action).await
        })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        true
    }

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Thermal, RedfishError>> {
        Box::pin(async move {
            let mut temperatures = Vec::new();
            let mut fans = Vec::new();
            let mut leak_detectors = Vec::new();

            // gb200 bianca has temperature sensors in several chassis items
            let chassis_all = self.s.get_chassis_all().await?;
            for chassis_id in chassis_all {
                let mut url = format!("Chassis/{}", chassis_id);
                let (_status_code, chassis): (StatusCode, Chassis) =
                    self.s.client.get(&url).await?;
                if chassis.thermal_subsystem.is_some() {
                    url = format!("Chassis/{}/ThermalSubsystem/ThermalMetrics", chassis_id);
                    let (_status_code, temps): (StatusCode, TemperaturesOemNvidia) =
                        self.s.client.get(&url).await?;
                    if let Some(temp) = temps.temperature_readings_celsius {
                        for t in temp {
                            let sensor: Temperature = Temperature::from(t);
                            temperatures.push(sensor);
                        }
                    }
                    // currently the gb200 bianca board we have uses liquid cooling
                    // walk through leak detection sensors and add those
                    url = format!(
                        "Chassis/{}/ThermalSubsystem/LeakDetection/LeakDetectors",
                        chassis_id
                    );

                    let res: Result<(StatusCode, Sensors), RedfishError> =
                        self.s.client.get(&url).await;

                    if let Ok((_, sensors)) = res {
                        for sensor in sensors.members {
                            url = sensor
                                .odata_id
                                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                            let (_status_code, l): (StatusCode, LeakDetector) =
                                self.s.client.get(&url).await?;
                            leak_detectors.push(l);
                        }
                    }
                }
                if chassis.sensors.is_some() {
                    // Special handling for GB200s that may not have all their drives installed but still have sensors
                    if let Some(backplane_num) = chassis_id.strip_prefix("StorageBackplane_") {
                        url = format!("Chassis/{}/Drives", chassis_id);

                        // Fetch drives and find their respective sensor
                        if let Ok((_status_code, drives)) =
                            self.s.client.get::<DriveCollection>(&url).await
                        {
                            for sensor in drives
                                .members
                                .iter()
                                .filter_map(|drive| {
                                    // Extract drive slot ID: "/path/NVMe_SSD_200" -> "200" -> 200
                                    let drive_id = drive
                                        .odata_id
                                        .split('/')
                                        .next_back()?
                                        .split('_')
                                        .next_back()?
                                        .parse::<u32>()
                                        .ok()?;

                                    Some((drive_id % 4, backplane_num))
                                })
                                .map(|(sensor_index, backplane)| {
                                    format!(
                                        "Chassis/{}/Sensors/StorageBackplane_{}_SSD_{}_Temp_0",
                                        chassis_id, backplane, sensor_index
                                    )
                                })
                            {
                                // Fetch sensor and add to temperatures if successful
                                if let Ok((_status_code, sensor_data)) =
                                    self.s.client.get::<Sensor>(&sensor).await
                                {
                                    temperatures.push(Temperature::from(sensor_data));
                                }
                            }
                        }
                    } else {
                        // walk through Chassis/*/Sensors/*/*Temp*/
                        url = format!("Chassis/{}/Sensors", chassis_id);
                        let (_status_code, sensors): (StatusCode, Sensors) =
                            self.s.client.get(&url).await?;
                        for sensor in sensors.members {
                            if !sensor.odata_id.contains("Temp") {
                                continue;
                            }
                            url = sensor
                                .odata_id
                                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                            let (_status_code, t): (StatusCode, Sensor) =
                                self.s.client.get(&url).await?;
                            let sensor: Temperature = Temperature::from(t);
                            temperatures.push(sensor);
                        }
                    }
                }

                // gb200 has fans under chassis sensors instead of thermal like other vendors, look for them in Chassis_0
                if chassis_id == *"Chassis_0" {
                    url = format!("Chassis/{}/Sensors", chassis_id);
                    let (_status_code, sensors): (StatusCode, Sensors) =
                        self.s.client.get(&url).await?;
                    for sensor in sensors.members {
                        if sensor.odata_id.contains("FAN") {
                            url = sensor
                                .odata_id
                                .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
                            let (_status_code, fan): (StatusCode, Fan) =
                                self.s.client.get(&url).await?;
                            fans.push(fan);
                        }
                    }
                }
            }
            let thermals = Thermal {
                temperatures,
                fans,
                leak_detectors: Some(leak_detectors),
                ..Default::default()
            };
            Ok(thermals)
        })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB200 has no sensors under Chassis/HGX_GPU_#/Sensors/".to_string(),
            ))
        })
    }

    fn get_system_event_log<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move { self.get_system_event_log().await })
    }

    fn get_bmc_event_log<'a>(
        &'a self,
        from: Option<chrono::DateTime<chrono::Utc>>,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Systems/{}/LogServices/EventLog/Entries",
                self.s.system_id()
            );
            self.s.fetch_bmc_event_log(url, from).await
        })
    }

    fn machine_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
        _bios_profiles: &'a HashMap<
            RedfishVendor,
            HashMap<String, HashMap<BiosProfileType, HashMap<String, serde_json::Value>>>,
        >,
        _selected_profile: BiosProfileType,
        _oem_manager_profiles: &'a HashMap<
            RedfishVendor,
            HashMap<String, HashMap<BiosProfileType, HashMap<String, serde_json::Value>>>,
        >,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let is_supermicro_gb300 = self.is_supermicro_gb300().await?;

            // The Supermicro GB300 SecureBoot resource does not expose
            // SecureBootEnable, so there is no supported setting to change.
            if !is_supermicro_gb300 {
                self.disable_secure_boot().await?;
            }

            let bios_attrs = self.machine_setup_attrs(is_supermicro_gb300).await?;
            let mut attrs = HashMap::new();
            attrs.extend(bios_attrs);
            let body = HashMap::from([("Attributes", attrs)]);
            let url = format!("Systems/{}/Bios/Settings", self.s.system_id());
            self.s
                .client
                .patch(&url, body)
                .await
                .map(|_status_code| None)
        })
    }

    fn machine_setup_status<'a>(
        &'a self,
        boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            // Resolve `InterfaceId` to a MAC via the Redfish-standard
            // EthernetInterface resource.
            let resolved_mac = match boot_interface {
                Some(b) => Some(crate::resolve_boot_interface_mac(self, b).await?),
                None => None,
            };
            let boot_interface_mac = resolved_mac.as_deref();

            // Check BIOS and BMC attributes
            let mut diffs = self.diff_bios_bmc_attr().await?;

            // Check the first boot option
            if let Some(mac) = boot_interface_mac {
                let (expected, actual) =
                    self.get_expected_and_actual_first_boot_option(mac).await?;
                if expected.is_none() || expected != actual {
                    diffs.push(MachineSetupDiff {
                        key: "boot_first".to_string(),
                        expected: expected.unwrap_or_else(|| "Not found".to_string()),
                        actual: actual.unwrap_or_else(|| "Not found".to_string()),
                    });
                }
            }

            // We don't lockdown on GB200, so we don't need to check for it

            Ok(MachineSetupStatus {
                is_done: diffs.is_empty(),
                diffs,
            })
        })
    }

    fn set_machine_password_policy<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            use serde_json::Value::Number;
            // These are also the defaults
            let body = HashMap::from([
                /* we were able to set AccountLockoutThreshold on the initial 3 GB200 trays we received
                   however, with the recent trays we received, it is not happy with setting a value of 0
                   for AccountLockoutThreshold: "The property 'AccountLockoutThreshold' with the requested value
                   of '0' could not be written because the value does not meet the constraints of the implementation."
                   Never lock
                  ("AccountLockoutThreshold", Number(0.into())),

                  instead, use the same threshold that we picked for vikings: the bmc will lock the account out after 4 attempts
                */
                ("AccountLockoutThreshold", Number(4.into())),
                // 600 is the smallest value it will accept. 10 minutes, in seconds.
                ("AccountLockoutDuration", Number(600.into())),
            ]);
            self.s
                .client
                .patch("AccountService", body)
                .await
                .map(|_status_code| ())
        })
    }

    fn lockdown<'a>(
        &'a self,
        _target: crate::EnabledDisabled,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            // OpenBMC does not provide a lockdown
            Ok(())
        })
    }

    fn boot_once<'a>(
        &'a self,
        target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let override_target = match target {
                crate::Boot::Pxe => BootSourceOverrideTarget::Pxe,
                crate::Boot::HardDisk => BootSourceOverrideTarget::Hdd,
                crate::Boot::UefiHttp => BootSourceOverrideTarget::UefiHttp,
            };
            Redfish::set_boot_override(
                self,
                BootOverride {
                    target: override_target,
                    enabled: BootSourceOverrideEnabled::Once,
                    mode: None,
                    http_boot_uri: None,
                },
            )
            .await?;
            Ok(())
        })
    }

    fn boot_first<'a>(
        &'a self,
        target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            match target {
                crate::Boot::Pxe => self.set_boot_order(BootOptionName::Pxe).await,
                crate::Boot::HardDisk => {
                    // We're looking for a UefiDevicePath like this:
                    // HD(1,GPT,A04D0F1E-E02F-4725-9434-0699B52D8FF2,0x800,0x100000)/\\EFI\\ubuntu\\shimaa64.efi
                    // The DisplayName will be something like "ubuntu".
                    let boot_array = self
                        .get_boot_options_ids_with_first(
                            BootOptionName::Hdd,
                            BootOptionMatchField::UefiDevicePath,
                            None,
                        )
                        .await?;
                    self.change_boot_order(boot_array).await
                }
                crate::Boot::UefiHttp => self.set_boot_order(BootOptionName::Http).await,
            }
        })
    }

    fn set_boot_override<'a>(
        &'a self,
        settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let mut boot_data: HashMap<String, serde_json::Value> = HashMap::new();
            boot_data.insert(
                "BootSourceOverrideTarget".to_string(),
                settings.target.to_string().into(),
            );
            boot_data.insert(
                "BootSourceOverrideEnabled".to_string(),
                settings.enabled.to_string().into(),
            );
            if let Some(mode) = settings.mode {
                boot_data.insert(
                    "BootSourceOverrideMode".to_string(),
                    mode.to_string().into(),
                );
            }
            if let Some(uri) = settings.http_boot_uri {
                boot_data.insert("HttpBootUri".to_string(), uri.into());
            }
            let url = format!("Systems/{}/Settings", self.s.system_id());
            self.s
                .client
                .patch(&url, HashMap::from([("Boot", boot_data)]))
                .await?;
            Ok(None)
        })
    }

    fn update_firmware_multipart<'a>(
        &'a self,
        filename: &'a Path,
        _reboot: bool,
        timeout: Duration,
        component_type: ComponentType,
    ) -> crate::RedfishFuture<'a, Result<String, RedfishError>> {
        Box::pin(async move {
            let firmware = File::open(&filename)
                .await
                .map_err(|e| RedfishError::FileError(format!("Could not open file: {}", e)))?;

            let update_service = self.s.get_update_service().await?;

            if update_service.multipart_http_push_uri.is_empty() {
                return Err(RedfishError::NotSupported(
                    "Host BMC does not support HTTP multipart push".to_string(),
                ));
            }

            let parameters = serde_json::to_string(&UpdateParameters::new(component_type))
                .map_err(|e| RedfishError::JsonSerializeError {
                    url: "".to_string(),
                    object_debug: "".to_string(),
                    source: e,
                })?;

            let (_status_code, _loc, body) = self
                .s
                .client
                .req_update_firmware_multipart(
                    filename,
                    firmware,
                    parameters,
                    &update_service.multipart_http_push_uri,
                    true,
                    timeout,
                )
                .await?;

            let task: Task =
                serde_json::from_str(&body).map_err(|e| RedfishError::JsonDeserializeError {
                    url: update_service.multipart_http_push_uri,
                    body,
                    source: e,
                })?;

            Ok(task.id)
        })
    }

    fn reset_bios<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.s.factory_reset_bios().await })
    }

    fn enable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if self.is_supermicro_gb300().await? {
                return Err(RedfishError::NotSupported(
                    "Supermicro GB300 does not expose SecureBootEnable".to_string(),
                ));
            }
            self.s.enable_secure_boot().await
        })
    }

    fn disable_secure_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if self.is_supermicro_gb300().await? {
                return Err(RedfishError::NotSupported(
                    "Supermicro GB300 does not expose SecureBootEnable".to_string(),
                ));
            }
            self.s.disable_secure_boot().await
        })
    }

    fn get_system_ethernet_interfaces<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Ok(vec![]) })
    }

    fn get_system_ethernet_interface<'a>(
        &'a self,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::EthernetInterface, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(format!(
                "GB200 doesn't have Systems EthernetInterface {id}"
            )))
        })
    }

    fn get_ports<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Chassis/{}/NetworkAdapters/{}/Ports",
                chassis_id, network_adapter
            );
            self.s.get_members(&url).await
        })
    }

    fn get_port<'a>(
        &'a self,
        chassis_id: &'a str,
        network_adapter: &'a str,
        id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::NetworkPort, RedfishError>> {
        Box::pin(async move {
            let url = format!(
                "Chassis/{}/NetworkAdapters/{}/Ports/{}",
                chassis_id, network_adapter, id
            );
            let (_status_code, body) = self.s.client.get(&url).await?;
            Ok(body)
        })
    }

    fn get_network_device_function<'a>(
        &'a self,
        _chassis_id: &'a str,
        _id: &'a str,
        _port: Option<&'a str>,
    ) -> crate::RedfishFuture<'a, Result<NetworkDeviceFunction, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB200 doesn't have Device Functions in NetworkAdapters yet".to_string(),
            ))
        })
    }

    /// http://redfish.dmtf.org/schemas/v1/NetworkDeviceFunctionCollection.json
    fn get_network_device_functions<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "GB200 doesn't have Device Functions in NetworkAdapters yet".to_string(),
            ))
        })
    }

    // Set current_uefi_password to "" if there isn't one yet. By default there isn't a password.
    /// Set new_uefi_password to "" to disable it.
    fn change_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
        new_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            self.s
                .change_bios_password(UEFI_PASSWORD_NAME, current_uefi_password, new_uefi_password)
                .await
        })
    }

    fn change_boot_order<'a>(
        &'a self,
        boot_array: Vec<String>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            let body = HashMap::from([("Boot", HashMap::from([("BootOrder", boot_array)]))]);
            let url = format!("Systems/{}/Settings", self.s.system_id());
            self.s.client.patch(&url, body).await?;
            Ok(())
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            let address = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            let mac_address = address.replace(':', "").to_uppercase();
            let boot_option_name =
                format!("{} (MAC:{})", BootOptionName::Http.to_string(), mac_address);
            let boot_array = self
                .get_boot_options_ids_with_first(
                    BootOptionName::Http,
                    BootOptionMatchField::DisplayName,
                    Some(&boot_option_name),
                )
                .await?;
            self.change_boot_order(boot_array).await?;
            Ok(None)
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            if self.is_supermicro_gb300().await? {
                return Err(RedfishError::NotSupported(
                    "Supermicro GB300 does not expose EmbeddedUefiShell".to_string(),
                ));
            }
            let attrs: HashMap<String, serde_json::Value> =
                HashMap::from([("EmbeddedUefiShell".to_string(), "Disabled".into())]);
            let body = HashMap::from([("Attributes", attrs)]);
            let url = format!("Systems/{}/Bios/Settings", self.s.system_id());
            self.s.client.patch(&url, body).await.map(|_status_code| ())
        })
    }

    fn is_infinite_boot_enabled<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Option<bool>, RedfishError>> {
        Box::pin(async move {
            if self.is_supermicro_gb300().await? {
                return Ok(None);
            }
            let embedded_uefi_shell = self.get_embedded_uefi_shell_status().await?;
            // Infinite boot is enabled when EmbeddedUefiShell is disabled
            Ok(Some(embedded_uefi_shell == EnabledDisabled::Disabled))
        })
    }

    fn is_boot_order_setup<'a>(
        &'a self,
        boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let mac = crate::resolve_boot_interface_mac(self, boot_interface).await?;
            let (expected, actual) = self.get_expected_and_actual_first_boot_option(&mac).await?;
            Ok(expected.is_some() && expected == actual)
        })
    }

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move {
            let diffs = self.diff_bios_bmc_attr().await?;
            Ok(diffs.is_empty())
        })
    }

    fn get_firmware_for_component<'a>(
        &'a self,
        component_integrity_id: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::software_inventory::SoftwareInventory, RedfishError>,
    > {
        Box::pin(async move {
            let mut id = None;

            for value in get_component_integrity_id_to_firmware_inventory_id_options()? {
                if let Some(capture) = value.pattern.captures(component_integrity_id) {
                    id = Some(format!(
                        "{}{}",
                        value.id_prefix,
                        capture
                            .get(1)
                            .ok_or_else(|| RedfishError::GenericError {
                                error: format!(
                                    "Empty capture for {}, id_prefix: {}",
                                    component_integrity_id, value.id_prefix
                                )
                            })?
                            .as_str()
                    ));
                    break;
                }
            }

            let Some(id) = id else {
                return Err(RedfishError::NotSupported(format!(
                    "No component match for {}",
                    component_integrity_id
                )));
            };
            self.get_firmware(&id).await
        })
    }
}

impl Bmc {
    /// Check BIOS and BMC attributes and return differences
    async fn diff_bios_bmc_attr(&self) -> Result<Vec<MachineSetupDiff>, RedfishError> {
        let mut diffs = vec![];

        // Check BIOS and BMC attributes
        let sb = self.get_secure_boot().await?;
        if sb.secure_boot_enable.unwrap_or(false) {
            diffs.push(MachineSetupDiff {
                key: "SecureBoot".to_string(),
                expected: "false".to_string(),
                actual: "true".to_string(),
            });
        }

        let bios = self.s.bios_attributes().await?;
        let is_supermicro_gb300 = self.is_supermicro_gb300().await?;
        let expected_attrs = self.machine_setup_attrs(is_supermicro_gb300).await?;
        for (key, expected) in expected_attrs {
            let Some(actual) = bios.get(&key) else {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    expected: expected.to_string(),
                    actual: "_missing_".to_string(),
                });
                continue;
            };
            // expected and actual are serde_json::Value which are not comparable, so to_string
            let act = actual.to_string();
            let exp = expected.to_string();
            if act != exp {
                diffs.push(MachineSetupDiff {
                    key: key.to_string(),
                    expected: exp,
                    actual: act,
                });
            }
        }

        Ok(diffs)
    }

    async fn get_expected_and_actual_first_boot_option(
        &self,
        boot_interface_mac: &str,
    ) -> Result<(Option<String>, Option<String>), RedfishError> {
        let mac_address = boot_interface_mac.replace(':', "").to_uppercase();
        let boot_option_name =
            format!("{} (MAC:{})", BootOptionName::Http.to_string(), mac_address);

        let boot_options = self.s.get_system().await?.boot.boot_order;

        // Get actual first boot option
        let actual_first_boot_option = if let Some(first) = boot_options.first() {
            Some(self.s.get_boot_option(first.as_str()).await?.display_name)
        } else {
            None
        };

        // Find expected boot option
        let mut expected_first_boot_option = None;
        for member in &boot_options {
            let b = self.s.get_boot_option(member.as_str()).await?;
            if b.display_name.starts_with(&boot_option_name) {
                expected_first_boot_option = Some(b.display_name);
                break;
            }
        }

        Ok((expected_first_boot_option, actual_first_boot_option))
    }

    // name: The name of the device you want to make the first boot choice.
    async fn set_boot_order(&self, name: BootOptionName) -> Result<(), RedfishError> {
        let boot_array = self
            .get_boot_options_ids_with_first(name, BootOptionMatchField::DisplayName, None)
            .await?;
        self.change_boot_order(boot_array).await
    }

    // This function searches all reported boot options to find the
    // desired option, then prepends it to the existing boot order.
    async fn get_boot_options_ids_with_first(
        &self,
        with_name: BootOptionName,
        match_field: BootOptionMatchField,
        with_name_str: Option<&str>,
    ) -> Result<Vec<String>, RedfishError> {
        let name_str = with_name_str.unwrap_or(with_name.to_string());
        let system = self.s.get_system().await?;

        let boot_options_id =
            system
                .boot
                .boot_options
                .clone()
                .ok_or_else(|| RedfishError::MissingKey {
                    key: "boot.boot_options".to_string(),
                    url: system.odata.odata_id.clone(),
                })?;

        let all_boot_options: Vec<BootOption> = self
            .get_collection(boot_options_id)
            .await
            .and_then(|c| c.try_get::<BootOption>())?
            .members;

        // Search through all boot options to find the one we want
        let found_boot_option = all_boot_options.iter().find(|b| match match_field {
            BootOptionMatchField::DisplayName => b.display_name.starts_with(name_str),
            BootOptionMatchField::UefiDevicePath => {
                matches!(&b.uefi_device_path, Some(x) if x.starts_with(name_str))
            }
        });

        let Some(target) = found_boot_option else {
            let all_names: Vec<_> = all_boot_options
                .iter()
                .map(|b| format!("{}: {}", b.id, b.display_name))
                .collect();
            return Err(RedfishError::GenericError {
                error: format!(
                    "Could not find boot option matching {name_str} on {}; all boot options: {:#?}",
                    match_field, all_names
                ),
            });
        };

        let target_id = target.id.clone();

        // Prepend the found option to the front of the existing boot order
        let mut ordered = system.boot.boot_order;
        ordered.retain(|id| id != &target_id);
        ordered.insert(0, target_id);

        Ok(ordered)
    }

    async fn get_system_event_log(&self) -> Result<Vec<LogEntry>, RedfishError> {
        let url = format!("Systems/{}/LogServices/SEL/Entries", self.s.system_id());
        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.s.client.get(&url).await?;
        let log_entries = log_entry_collection.members;
        Ok(log_entries)
    }

    async fn machine_setup_attrs(
        &self,
        is_supermicro_gb300: bool,
    ) -> Result<Vec<(String, serde_json::Value)>, RedfishError> {
        let mut bios_attrs = machine_setup_bios_attrs(is_supermicro_gb300);
        let current_bios_attributes = self.s.bios_attributes().await?;

        // Enable Option ROM so that the DPU will show up in the Host's network devce list
        // Otherwise, we will never see the DPU's Host PF MAC in the boot option list
        if let Some(curr_bios_attributes) = current_bios_attributes.as_object() {
            for attribute in curr_bios_attributes.keys() {
                if attribute.contains("Pcie6DisableOptionROM") {
                    bios_attrs.push((attribute.into(), false.into()));
                }
            }
        }

        Ok(bios_attrs)
    }

    // get_embedded_uefi_shell_status returns the current status of the EmbeddedUefiShell BIOS attribute.
    async fn get_embedded_uefi_shell_status(&self) -> Result<EnabledDisabled, RedfishError> {
        let url = format!("Systems/{}/Bios", self.s.system_id());
        let bios_value = self.s.bios_attributes().await?;
        let bios_attributes =
            bios_value
                .as_object()
                .ok_or_else(|| RedfishError::InvalidKeyType {
                    key: "Attributes".to_string(),
                    expected_type: "object".to_string(),
                    url: url.clone(),
                })?;

        let embedded_uefi_shell = jsonmap::get_str(bios_attributes, "EmbeddedUefiShell", &url)?;

        match embedded_uefi_shell {
            "Enabled" => Ok(EnabledDisabled::Enabled),
            "Disabled" => Ok(EnabledDisabled::Disabled),
            _ => Err(RedfishError::InvalidValue {
                url,
                field: "EmbeddedUefiShell".to_string(),
                err: crate::model::InvalidValueError(format!(
                    "Expected 'Enabled' or 'Disabled', got '{}'",
                    embedded_uefi_shell
                )),
            }),
        }
    }
}

fn machine_setup_bios_attrs(is_supermicro_gb300: bool) -> Vec<(String, serde_json::Value)> {
    if is_supermicro_gb300 {
        // Supermicro GB300 exposes TPM through this AMI BIOS attribute.
        vec![("SecurityDeviceSupport".into(), "Enabled".into())]
    } else {
        vec![
            // NVIDIA GB200/GB300 exposes TPM directly.
            ("TPM".into(), "Enabled".into()),
            // Disable EmbeddedUefiShell (infinite boot workaround).
            ("EmbeddedUefiShell".into(), "Disabled".into()),
        ]
    }
}

// UpdateParameters is what is sent for a multipart firmware upload's metadata.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct UpdateParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    targets: Option<Vec<String>>,
    force_update: bool,
}

impl UpdateParameters {
    pub fn new(component: ComponentType) -> UpdateParameters {
        let targets = match component {
            ComponentType::Unknown => None,
            ComponentType::BMC => Some(vec![]),
            ComponentType::EROTBMC => Some(vec!["/redfish/v1/Chassis/HGX_ERoT_BMC_0".to_string()]),
            ComponentType::EROTBIOS => Some(vec![
                "/redfish/v1/UpdateService/FirmwareInventory/EROT_BIOS_0".to_string(),
            ]),
            ComponentType::HGXBMC | ComponentType::UEFI => {
                Some(vec!["/redfish/v1/Chassis/HGX_Chassis_0".to_string()])
            }
            _ => Some(vec!["unreachable".to_string()]),
        };

        UpdateParameters {
            targets,
            force_update: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gb300_machine_setup_uses_vendor_specific_bios_attributes() {
        let dgx_attrs = machine_setup_bios_attrs(false);
        assert!(dgx_attrs.contains(&("TPM".into(), "Enabled".into())));
        assert!(dgx_attrs.contains(&("EmbeddedUefiShell".into(), "Disabled".into())));
        assert!(!dgx_attrs
            .iter()
            .any(|(key, _)| key == "SecurityDeviceSupport"));

        let supermicro_attrs = machine_setup_bios_attrs(true);
        assert_eq!(
            supermicro_attrs,
            vec![("SecurityDeviceSupport".into(), "Enabled".into())]
        );
    }

    #[test]
    fn systems_are_supermicro_gb300_only_for_supermicro_hardware() {
        let dgx_systems = vec![ComputerSystem {
            manufacturer: Some("NVIDIA".into()),
            model: Some("GB300 NVL".into()),
            ..Default::default()
        }];
        assert!(!systems_are_supermicro_gb300(&dgx_systems));

        let supermicro_systems = vec![ComputerSystem {
            manufacturer: Some("Supermicro".into()),
            model: Some("GB300 NVL".into()),
            ..Default::default()
        }];
        assert!(systems_are_supermicro_gb300(&supermicro_systems));

        let split_systems = vec![
            ComputerSystem {
                id: "System_0".into(),
                manufacturer: Some("Supermicro".into()),
                model: Some("GB NVL".into()),
                ..Default::default()
            },
            ComputerSystem {
                id: "HGX_Baseboard_0".into(),
                manufacturer: Some("NVIDIA".into()),
                model: Some("GB300 1CPU:2GPU Board PC".into()),
                ..Default::default()
            },
        ];
        assert!(systems_are_supermicro_gb300(&split_systems));
    }

    #[test]
    fn test_update_parameters_targets_all_variants() {
        let cases: Vec<(ComponentType, Option<Vec<String>>)> = vec![
            (ComponentType::Unknown, None),
            (ComponentType::BMC, Some(vec![])),
            (
                ComponentType::EROTBMC,
                Some(vec!["/redfish/v1/Chassis/HGX_ERoT_BMC_0".to_string()]),
            ),
            (
                ComponentType::EROTBIOS,
                Some(vec![
                    "/redfish/v1/UpdateService/FirmwareInventory/EROT_BIOS_0".to_string(),
                ]),
            ),
            (
                ComponentType::HGXBMC,
                Some(vec!["/redfish/v1/Chassis/HGX_Chassis_0".to_string()]),
            ),
            (
                ComponentType::UEFI,
                Some(vec!["/redfish/v1/Chassis/HGX_Chassis_0".to_string()]),
            ),
            (
                ComponentType::CPLDMID,
                Some(vec!["unreachable".to_string()]),
            ),
            (ComponentType::CPLDMB, Some(vec!["unreachable".to_string()])),
            (
                ComponentType::CPLDPDB,
                Some(vec!["unreachable".to_string()]),
            ),
            (
                ComponentType::PSU { num: 1 },
                Some(vec!["unreachable".to_string()]),
            ),
            (
                ComponentType::PCIeSwitch { num: 2 },
                Some(vec!["unreachable".to_string()]),
            ),
            (
                ComponentType::PCIeRetimer { num: 3 },
                Some(vec!["unreachable".to_string()]),
            ),
        ];

        for (component, expected_targets) in cases {
            let params = UpdateParameters::new(component.clone());
            assert_eq!(
                params.targets, expected_targets,
                "Failed for component: {:?}",
                component
            );
            assert!(
                params.force_update,
                "Force update not true for: {:?}",
                component
            );
        }
    }
}
