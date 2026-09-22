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
use std::collections::HashMap;
use std::fmt;

use model::{OData, ODataId};
use serde::{Deserialize, Serialize};

use crate::model;

/// https://redfish.dmtf.org/schemas/v1/ServiceRoot.v1_16_0.json
/// This type shall contain information about deep operations that the service supports.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ServiceRoot {
    #[serde(flatten)]
    pub odata: OData,
    pub id: Option<String>,
    pub name: Option<String>,
    pub product: Option<String>,
    pub redfish_version: String,
    pub vendor: Option<String>,
    #[serde(rename = "UUID")]
    pub uuid: Option<String>,
    pub oem: Option<HashMap<String, serde_json::Value>>,
    pub update_service: Option<HashMap<String, serde_json::Value>>,
    pub account_service: Option<ODataId>,
    pub certificate_service: Option<ODataId>,
    pub chassis: Option<ODataId>,
    pub component_integrity: Option<ODataId>,
    pub event_service: Option<ODataId>,
    pub license_service: Option<ODataId>,
    pub fabrics: Option<ODataId>,
    pub managers: Option<ODataId>,
    pub session_service: Option<ODataId>,
    pub systems: Option<ODataId>,
    pub tasks: Option<ODataId>,
    pub telemetry_service: Option<ODataId>,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy, Debug, PartialEq, Hash, Eq, Serialize, Deserialize)]
pub enum RedfishVendor {
    Lenovo,
    LenovoAMI,
    LenovoGB300,
    Dell,
    NvidiaDpu,
    Supermicro,
    AMI, // Viking DGX H100
    Hpe,
    NvidiaGH200, // grace-hopper 200
    NvidiaGBx00, // all Grace-Blackwell combinations 200, .. since openbmc fw and redfish schema are the same
    VeraRubin,
    NvidiaGBSwitch, // GB NVLink switch
    P3809, // dummy for P3809, needs to be set to NvidiaGH200 or NvidiaGBSwitch based on chassis
    LiteOnPowerShelf,
    DeltaPowerShelf,
    Sushy,
    Unknown,
}

impl fmt::Display for RedfishVendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl ServiceRoot {
    /// Vendor provided by Redfish ServiceRoot
    pub fn vendor_string(&self) -> Option<String> {
        // If there is no "Vendor" key in ServiceRoot, look for an "Oem" entry. It will have a
        // single key which is the vendor name.
        self.vendor
            .as_ref()
            .cloned()
            .or_else(|| match &self.oem {
                Some(oem) => oem.keys().next().cloned(),
                None => None,
            })
            .or_else(|| {
                // Stock Sushy emulator has no Vendor or Oem fields but uses
                // "RedvirtService" as its Id. Detect it from Id and Name so
                // auto-detection works without a custom root.json template.
                let haystack = [self.id.as_deref(), self.name.as_deref()];
                let is_sushy = haystack.iter().flatten().any(|s| {
                    let lower = s.to_lowercase();
                    lower.contains("sushy") || lower.contains("redvirt")
                });
                if is_sushy {
                    Some("Sushy".to_string())
                } else {
                    None
                }
            })
    }

    pub fn vendor(&self) -> Option<RedfishVendor> {
        let v = self.vendor_string().unwrap_or("Unknown".to_string());
        Some(match v.to_lowercase().as_str() {
            "ami" => RedfishVendor::AMI,
            "dell" => RedfishVendor::Dell,
            "hpe" => RedfishVendor::Hpe,
            "lenovo" => {
                if self.has_ami_bmc() {
                    RedfishVendor::LenovoAMI
                } else {
                    RedfishVendor::Lenovo
                }
            }
            "nvidia" => match self.product.as_deref() {
                Some("P3809") => RedfishVendor::P3809, // could be gh200 compute or nvswitch
                Some("VR NVL72") => RedfishVendor::VeraRubin,
                Some("GB200 NVL") | Some("GB BMC") => RedfishVendor::NvidiaGBx00,
                _ => RedfishVendor::NvidiaDpu,
            },
            "wiwynn" => RedfishVendor::NvidiaGBx00,
            "supermicro" => match self.product.as_deref() {
                Some("GB NVL") => RedfishVendor::NvidiaGBx00,
                _ => RedfishVendor::Supermicro,
            },
            "lite-on technology corp." => RedfishVendor::LiteOnPowerShelf,
            "delta electronics inc." => RedfishVendor::DeltaPowerShelf,
            "sushy" | "contoso" | "redvirt" => RedfishVendor::Sushy,
            _ => RedfishVendor::Unknown,
        })
    }

    /// Vera Rubin compute-tray host BMC.
    pub fn is_vera_rubin(&self) -> bool {
        self.vendor() == Some(RedfishVendor::VeraRubin)
    }

    /// Check if this system has an AMI-based BMC (indicated by "Ami" key in OEM field)
    pub fn has_ami_bmc(&self) -> bool {
        self.oem
            .as_ref()
            .map(|oem| oem.keys().any(|k| k.to_lowercase() == "ami"))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod test {
    use crate::model::service_root::{RedfishVendor, ServiceRoot};

    #[test]
    fn test_supermicro_service_root() {
        let data = include_str!("testdata/supermicro_service_root.json");
        let result: super::ServiceRoot = serde_json::from_str(data).unwrap();
        assert_eq!(result.vendor().unwrap(), RedfishVendor::Supermicro);
    }

    #[test]
    fn test_nvidia_gb_bmc_service_root() {
        let result = ServiceRoot {
            vendor: Some("NVIDIA".to_string()),
            product: Some("GB BMC".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::NvidiaGBx00);
    }

    #[test]
    fn test_supermicro_gb300_service_root() {
        let result = ServiceRoot {
            vendor: Some("Supermicro".to_string()),
            product: Some("GB NVL".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::NvidiaGBx00);
    }

    #[test]
    fn test_nvidia_bluefield_service_root() {
        let result = ServiceRoot {
            vendor: Some("NVIDIA".to_string()),
            product: Some("BlueField-3".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::NvidiaDpu);
    }

    #[test]
    fn test_nvidia_vera_rubin_service_root() {
        let result = ServiceRoot {
            vendor: Some("NVIDIA".to_string()),
            product: Some("VR NVL72".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::VeraRubin);
        assert!(result.is_vera_rubin());
    }

    #[test]
    fn test_delta_powershelf_service_root() {
        // Real Delta power shelves report their full manufacturer string in the
        // service-root `Vendor` field, not a bare "Delta".
        let result = ServiceRoot {
            vendor: Some("Delta Electronics Inc.".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::DeltaPowerShelf);
    }

    #[test]
    fn test_delta_powershelf_service_root_case_insensitive() {
        // Matching is case-insensitive (the vendor string is lowercased first).
        let result = ServiceRoot {
            vendor: Some("DELTA ELECTRONICS INC.".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::DeltaPowerShelf);
    }

    #[test]
    fn test_sushy_detected_from_vendor_field() {
        let result = ServiceRoot {
            vendor: Some("Sushy".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::Sushy);
    }

    #[test]
    fn test_sushy_detected_from_id_fallback() {
        // Stock Sushy 2.2.0 has no Vendor field but uses "RedvirtService" as Id.
        let result = ServiceRoot {
            id: Some("RedvirtService".to_string()),
            name: Some("Redvirt Service".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::Sushy);
    }

    #[test]
    fn test_sushy_detected_from_name_fallback() {
        let result = ServiceRoot {
            name: Some("Sushy Service".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::Sushy);
    }

    #[test]
    fn test_sushy_detected_from_name_when_id_is_generic() {
        let result = ServiceRoot {
            id: Some("RootService".to_string()),
            name: Some("Redvirt Service".to_string()),
            ..Default::default()
        };
        assert_eq!(result.vendor().unwrap(), RedfishVendor::Sushy);
    }
}
