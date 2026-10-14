use crate::Assembly;
use std::collections::HashMap;

use crate::model::boot::BootOverride;
use crate::model::certificate::Certificate;
use crate::model::component_integrity::ComponentIntegrities;
use crate::model::power::{PowerShelf, PowerShelves};
use crate::model::sensor::GPUSensors;
use crate::model::service_root::RedfishVendor;
use crate::model::task::Task;
use crate::network::REDFISH_ENDPOINT;
use crate::MachineSetupStatus;
use crate::{
    model::{
        chassis::NetworkAdapter,
        sel::{LogEntry, LogEntryCollection},
        BootOption, ComputerSystem,
    },
    standard::RedfishStandard,
    BiosProfileType, NetworkDeviceFunction, Redfish, RedfishError,
};

pub struct Bmc {
    s: RedfishStandard,
}

impl Bmc {
    pub fn new(s: RedfishStandard) -> Result<Bmc, RedfishError> {
        Ok(Bmc { s })
    }
}
impl Redfish for Bmc {
    fn std_redfish(&self) -> &RedfishStandard {
        &self.s
    }
    fn get_power_state<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::PowerState, RedfishError>> {
        Box::pin(async move { self.derive_power_state().await })
    }

    fn get_power_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Power, RedfishError>> {
        // Discover the chassis carrying the PowerSubsystem rather than
        // hard-coding the Delta-specific id.
        Box::pin(async move { self.s.get_power_metrics_from_power_subsystem().await })
    }

    fn power<'a>(
        &'a self,
        action: crate::SystemPowerControl,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { self.set_psu_power(action).await })
    }

    fn get_thermal_metrics<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::Thermal, RedfishError>> {
        // Delta exposes temperatures under ThermalSubsystem/ThermalMetrics
        // rather than the legacy /Thermal resource. Discover the chassis
        // carrying the ThermalSubsystem rather than hard-coding the id.
        Box::pin(async move { self.s.get_thermal_metrics_from_thermal_subsystem().await })
    }

    fn get_gpu_sensors<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<GPUSensors>, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("no gpus".to_string())) })
    }

    fn get_system_event_log<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<LogEntry>, RedfishError>> {
        Box::pin(async move { self.get_system_event_log().await })
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
            // we don't do any changes for powershelves
            Ok(None)
        })
    }

    fn machine_setup_status<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<MachineSetupStatus, RedfishError>> {
        Box::pin(async move {
            let diffs = vec![];
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
                // Never lock
                ("AccountLockoutThreshold", Number(10.into())),
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
        Box::pin(async move { Ok(()) })
    }

    fn get_boot_options<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<crate::BootOptions, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn get_boot_option<'a>(
        &'a self,
        _option_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<BootOption, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn boot_once<'a>(
        &'a self,
        _target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn boot_first<'a>(
        &'a self,
        _target: crate::Boot,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn set_boot_override<'a>(
        &'a self,
        _settings: BootOverride,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf does not support boot source overrides".to_string(),
            ))
        })
    }

    fn pcie_devices<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<crate::PCIeDevice>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf doesn't have PCIeDevices tree".to_string(),
            ))
        })
    }

    /// Delta power shelves expose no `/Systems` resource, but the site
    /// explorer's report path still calls `get_system`. Rather than 404 on
    /// `/Systems/{id}`, synthesize a minimal `ComputerSystem` from the chassis
    /// (manufacturer/model/serial) and derive the power state from the PSU OEM
    /// flags
    fn get_system<'a>(&'a self) -> crate::RedfishFuture<'a, Result<ComputerSystem, RedfishError>> {
        Box::pin(async move {
            let chassis_ids = self.s.get_chassis_all().await?;
            let chassis_id = chassis_ids
                .first()
                .cloned()
                .unwrap_or_else(|| "chassis".to_string());
            let chassis = self.s.get_chassis(&chassis_id).await?;
            let power_state = self.derive_power_state().await?;
            Ok(ComputerSystem {
                id: chassis_id,
                manufacturer: chassis.manufacturer,
                model: chassis.model,
                serial_number: chassis.serial_number,
                power_state,
                ..Default::default()
            })
        })
    }

    fn add_secure_boot_certificate<'a>(
        &'a self,
        _pem_cert: &'a str,
        _database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf secure boot unsupported".to_string(),
            ))
        })
    }

    fn get_chassis_network_adapters<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf doesn't support NetworkAdapters tree".to_string(),
            ))
        })
    }

    fn get_chassis_network_adapter<'a>(
        &'a self,
        _chassis_id: &'a str,
        _id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<NetworkAdapter, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf doesn't have NetworkAdapters tree".to_string(),
            ))
        })
    }

    fn get_system_ethernet_interfaces<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Ok(vec![]) })
    }

    fn get_system_ethernet_interface<'a>(
        &'a self,
        _id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::EthernetInterface, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf doesn't have Systems EthernetInterface".to_string(),
            ))
        })
    }

    fn get_ports<'a>(
        &'a self,
        _chassis_id: &'a str,
        _network_adapter: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf doesn't have NetworkAdapters tree".to_string(),
            ))
        })
    }

    fn get_port<'a>(
        &'a self,
        _chassis_id: &'a str,
        _network_adapter: &'a str,
        _id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::NetworkPort, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf doesn't have NetworkAdapters tree".to_string(),
            ))
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
                "Delta powershelf doesn't have NetworkAdapters tree".to_string(),
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
                "Delta powershelf doesn't have NetworkAdapters tree".to_string(),
            ))
        })
    }

    /// Delta power shelves have no UEFI/BIOS, so there is no UEFI password to
    /// manage.
    fn change_uefi_password<'a>(
        &'a self,
        _current_uefi_password: &'a str,
        _new_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf UEFI password unsupported".to_string(),
            ))
        })
    }

    fn change_boot_order<'a>(
        &'a self,
        _boot_array: Vec<String>,
    ) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "Delta powershelf does not support changing boot order".to_string(),
            ))
        })
    }

    fn set_boot_order_dpu_first<'a>(
        &'a self,
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move {
            Err(RedfishError::NotSupported(
                "set_dpu_first_boot_order".to_string(),
            ))
        })
    }

    fn clear_uefi_password<'a>(
        &'a self,
        current_uefi_password: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Option<String>, RedfishError>> {
        Box::pin(async move { self.change_uefi_password(current_uefi_password, "").await })
    }

    fn get_secure_boot_certificate<'a>(
        &'a self,
        _database_id: &'a str,
        _certificate_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Certificate, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_secure_boot_certificates<'a>(
        &'a self,
        _database_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Vec<String>, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn is_bios_setup<'a>(
        &'a self,
        _boot_interface: Option<crate::BootInterfaceRef<'a>>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn enable_infinite_boot<'a>(&'a self) -> crate::RedfishFuture<'a, Result<(), RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn trigger_evidence_collection<'a>(
        &'a self,
        _url: &'a str,
        _nonce: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Task, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_evidence<'a>(
        &'a self,
        _url: &'a str,
    ) -> crate::RedfishFuture<'a, Result<crate::model::component_integrity::Evidence, RedfishError>>
    {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_firmware_for_component<'a>(
        &'a self,
        _component_integrity_id: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::software_inventory::SoftwareInventory, RedfishError>,
    > {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_component_ca_certificate<'a>(
        &'a self,
        _url: &'a str,
    ) -> crate::RedfishFuture<
        'a,
        Result<crate::model::component_integrity::CaCertificate, RedfishError>,
    > {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn get_chassis_assembly<'a>(
        &'a self,
        _chassis_id: &'a str,
    ) -> crate::RedfishFuture<'a, Result<Assembly, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }

    fn ac_powercycle_supported_by_power(&self) -> bool {
        false
    }

    /// Delta power shelves have no boot order, so there is nothing to set up.
    fn is_boot_order_setup<'a>(
        &'a self,
        _boot_interface: crate::BootInterfaceRef<'a>,
    ) -> crate::RedfishFuture<'a, Result<bool, RedfishError>> {
        Box::pin(async move { Ok(true) })
    }

    fn get_component_integrities<'a>(
        &'a self,
    ) -> crate::RedfishFuture<'a, Result<ComponentIntegrities, RedfishError>> {
        Box::pin(async move { Err(RedfishError::NotSupported("not supported".to_string())) })
    }
}

impl Bmc {
    async fn get_system_event_log(&self) -> Result<Vec<LogEntry>, RedfishError> {
        let url = format!(
            "Managers/{}/LogServices/EventLog/Entries",
            self.s.manager_id()
        );
        let (_status_code, log_entry_collection): (_, LogEntryCollection) =
            self.s.client.get(&url).await?;
        let log_entries = log_entry_collection.members;
        Ok(log_entries)
    }

    /// Delta power shelves have no `/Systems` resource and therefore no
    /// `ComputerSystem.Reset`. Power control is exposed as OEM actions on the
    /// shelf (`PowerEquipment/PowerShelves/<id>`): `#PowerShelf.TurnOnPSUs` /
    /// `#PowerShelf.TurnOffPSUs`, which switch the shelf's PSU outputs on/off.
    ///
    /// There is no native restart action, so the restart/power-cycle variants
    /// are reported as unsupported rather than synthesizing an off/on sequence
    /// that would hard power-cycle everything downstream.
    async fn set_psu_power(&self, action: crate::SystemPowerControl) -> Result<(), RedfishError> {
        use crate::SystemPowerControl::*;
        let turn_on = match action {
            On => true,
            ForceOff | GracefulShutdown => false,
            GracefulRestart | ForceRestart | ACPowercycle | PowerCycle => {
                return Err(RedfishError::NotSupported(
                    "Delta powershelf supports only PSU on/off, not restart/power-cycle"
                        .to_string(),
                ));
            }
        };

        let actions = self
            .get_power_shelf()
            .await?
            .oem
            .and_then(|oem| oem.deltaenergysystems)
            .and_then(|delta| delta.actions)
            .ok_or_else(|| {
                RedfishError::NotSupported(
                    "Delta powershelf does not advertise PSU on/off actions".to_string(),
                )
            })?;

        let action = if turn_on {
            actions.turn_on_psus
        } else {
            actions.turn_off_psus
        };
        let target = action.and_then(|a| a.target).ok_or_else(|| {
            RedfishError::NotSupported(
                "Delta powershelf does not advertise the requested PSU action".to_string(),
            )
        })?;

        // The TurnOn/TurnOffPSUs OEM actions take no parameters.
        let url = target.replace(&format!("/{REDFISH_ENDPOINT}/"), "");
        self.s
            .client
            .post(&url, HashMap::<String, String>::new())
            .await
            .map(|_| ())
    }

    /// Fetch the first `PowerEquipment/PowerShelves` member. Delta exposes a
    /// single shelf, but discover its id rather than hard-coding it.
    async fn get_power_shelf(&self) -> Result<PowerShelf, RedfishError> {
        let (_, shelves): (_, PowerShelves) =
            self.s.client.get("PowerEquipment/PowerShelves/").await?;
        let member = shelves
            .members
            .first()
            .ok_or_else(|| RedfishError::GenericError {
                error: "No power shelves found under PowerEquipment".to_string(),
            })?;
        let url = member
            .odata_id
            .replace(&format!("/{REDFISH_ENDPOINT}/"), "");
        let (_, shelf): (_, PowerShelf) = self.s.client.get(&url).await?;
        Ok(shelf)
    }

    /// Derives the overall powershelf `PowerState` from the per-PSU
    /// `Oem.deltaenergysystems.Power` flag (Delta exposes no `PowerState`
    /// field on the PowerSupply, nor a `/Systems` resource).
    ///
    /// All PSUs report `Power: true` → On, all `false` → Off, anything mixed,
    /// missing, or unreadable → Unknown.
    async fn derive_power_state(&self) -> Result<crate::PowerState, RedfishError> {
        // get_power_metrics already fetches every PSU; reuse it and read the
        // typed Delta OEM on/off flag rather than re-fetching here.
        let power = self.get_power_metrics().await?;
        let states: Vec<Option<bool>> = power
            .power_supplies
            .unwrap_or_default()
            .iter()
            .map(|ps| ps.is_delta_psu_on())
            .collect();

        if states.is_empty() || states.iter().any(Option::is_none) {
            return Ok(crate::PowerState::Unknown);
        }
        if states.iter().all(|s| *s == Some(true)) {
            Ok(crate::PowerState::On)
        } else if states.iter().all(|s| *s == Some(false)) {
            Ok(crate::PowerState::Off)
        } else {
            Ok(crate::PowerState::Unknown)
        }
    }
}
