// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: MIT

#[test]
fn partial_status_can_be_constructed_by_external_test_clients() {
    let status = libredfish::Status::build_fake_partial();
    assert!(status.is_partially_enabled());
    assert!(!status.is_fully_enabled());
    assert!(!status.is_fully_disabled());
}
