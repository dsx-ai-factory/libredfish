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
use serde::{Deserialize, Serialize};

use super::oem::PortExtensions;
use super::{InvalidValueError, LinkStatus, ODataLinks};
use crate::RedfishError;

#[derive(Debug, Serialize, Deserialize, Copy, Clone, Eq, PartialEq)]
pub enum LinkNetworkTechnology {
    Ethernet,
    InfiniBand,
    FibreChannel,
}

impl std::fmt::Display for LinkNetworkTechnology {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

/// https://redfish.dmtf.org/schemas/v1/Port.v1_6_0.json
/// `NetworkPort` contains the physical port information exposed by the
/// current Redfish `Port` schema.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkPort {
    #[serde(flatten)]
    pub odata: Option<ODataLinks>,
    pub description: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub link_status: Option<LinkStatus>,
    pub link_network_technology: Option<LinkNetworkTechnology>,
    pub current_speed_gbps: Option<f64>,
    pub ethernet: Option<PortEthernet>,
    pub oem: Option<PortExtensions>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PortEthernet {
    #[serde(default, rename = "AssociatedMACAddresses")]
    pub associated_mac_addresses: Vec<String>,
}

impl NetworkPort {
    /// `mac_addresses` returns the standard port MAC addresses, falling back
    /// to Lenovo's physical-port address when XCC omits the standard fields.
    pub fn mac_addresses(&self) -> Result<Vec<mac_address::MacAddress>, RedfishError> {
        let (field, addresses) = if let Some(standard_addresses) = self
            .ethernet
            .as_ref()
            .map(|ethernet| ethernet.associated_mac_addresses.as_slice())
            .filter(|addresses| !addresses.is_empty())
        {
            ("Ethernet.AssociatedMACAddresses", standard_addresses)
        } else {
            (
                "Oem.Lenovo.PhysicalPortMacAddress",
                self.oem
                    .as_ref()
                    .and_then(|oem| oem.lenovo.as_ref())
                    .and_then(|lenovo| lenovo.physical_port_mac_address.as_ref())
                    .map(std::slice::from_ref)
                    .unwrap_or_default(),
            )
        };

        addresses
            .iter()
            .map(|address| {
                address.parse().map_err(|err: mac_address::MacParseError| {
                    RedfishError::InvalidValue {
                        url: self
                            .odata
                            .as_ref()
                            .map(|odata| odata.odata_id.clone())
                            .unwrap_or_else(|| "Port".to_string()),
                        field: field.to_string(),
                        err: InvalidValueError(err.to_string()),
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::NetworkPort;

    #[test]
    fn mac_addresses_use_standard_port_data_then_lenovo_fallback() {
        let cases = [
            (
                "standard port data takes precedence",
                r#"{
                    "Ethernet": {
                        "AssociatedMACAddresses": ["02:aa:bb:cc:dd:01"]
                    },
                    "Oem": {
                        "Lenovo": {
                            "PhysicalPortMacAddress": "not-a-mac"
                        }
                    }
                }"#,
                "02:aa:bb:cc:dd:01",
            ),
            (
                "Lenovo OEM port",
                r#"{
                    "Oem": {
                        "Lenovo": {
                            "PhysicalPortMacAddress": "02AABBCCDD02"
                        }
                    }
                }"#,
                "02:aa:bb:cc:dd:02",
            ),
        ];

        for (name, json, expected) in cases {
            let port: NetworkPort = serde_json::from_str(json).unwrap();
            let addresses = port.mac_addresses().unwrap();
            assert_eq!(
                addresses,
                vec![expected.parse().unwrap()],
                "unexpected addresses for {name}"
            );
        }
    }

    #[test]
    fn current_speed_accepts_redfish_number() {
        let json = include_str!(
            "../../tests/mockups/hpe/redfish/v1/Chassis/1/NetworkAdapters/DE084000/Ports/0/index.json"
        );
        let port: NetworkPort = serde_json::from_str(json).unwrap();
        assert_eq!(port.current_speed_gbps, Some(100.0));
    }
}
