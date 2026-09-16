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
use reqwest::StatusCode;

use crate::model::InvalidValueError;

#[derive(thiserror::Error, Debug)]
pub enum RedfishError {
    #[error("Network error talking to BMC at {url}. {source}")]
    NetworkError { url: String, source: reqwest::Error },

    #[error("HTTP {status_code} at {url}: {response_body}")]
    HTTPErrorCode {
        url: String,
        status_code: StatusCode,
        response_body: String,
    },

    #[error("Could not deserialize response from {url}. Body: {body}. {source}")]
    JsonDeserializeError {
        url: String,
        body: String,
        source: serde_json::Error,
    },

    #[error("Could not serialize request body for {url}. Obj: {object_debug}. {source}")]
    JsonSerializeError {
        url: String,
        object_debug: String,
        source: serde_json::Error,
    },

    #[error("Remote returned empty body")]
    NoContent,

    #[error("Remote returned empty header")]
    NoHeader,

    #[error("No such boot option {0}")]
    MissingBootOption(String),

    #[error("UnnecessaryOperation such as trying to turn on a machine that is already on.")]
    UnnecessaryOperation,

    #[error("Missing key {key} in JSON at {url}")]
    MissingKey { key: String, url: String },

    #[error("Key {key} should be {expected_type} at {url}")]
    InvalidKeyType {
        key: String,
        expected_type: String,
        url: String,
    },

    #[error("Field {field} parse error at {url}: {err}")]
    InvalidValue {
        url: String,
        field: String,
        err: InvalidValueError,
    },

    #[error("BMC is locked down, operation cannot be applied. Disable lockdown and retry.")]
    Lockdown,

    #[error("BMC vendor does not support this operation: {0}")]
    NotSupported(String),

    #[error("Could not find user with UserName matching '{0}'")]
    UserNotFound(String),

    #[error("Reqwest error: '{0}'")]
    ReqwestError(#[from] reqwest::Error),

    #[error("Issue with file: {0}")]
    FileError(String),

    #[error("Could not identify BMC vendor")]
    MissingVendor,

    #[error("Password change required")]
    PasswordChangeRequired { account_uri: Option<String> },

    #[error("Maximum amount of user accounts reached. Delete one to continue.")]
    TooManyUsers,

    #[error("Expected type: {expected}, actual: {actual}. Resource type: {resource_type}, resource uri: {resource_uri}")]
    TypeMismatch {
        expected: String,
        actual: String,
        resource_type: String,
        resource_uri: String,
    },

    #[error("DPU not found")]
    NoDpu, // suport zero-dpu, but warn about it too

    #[error("Error: {error}")]
    GenericError { error: String },
}

impl RedfishError {
    /// Returns `true` if the operation failed due to missing authentication or
    /// invalid credentials
    ///
    /// This is method on `RedfishError` in order to preserve the full error
    /// details in `RedfishError::HttpErrorCode`
    pub fn is_unauthorized(&self) -> bool {
        // clippy wants use of matches! macro
        matches!(self, RedfishError::HTTPErrorCode {
                url: _,
                status_code,
                response_body: _,
            } if *status_code == StatusCode::UNAUTHORIZED
                || *status_code == StatusCode::FORBIDDEN)
    }

    pub fn not_found(&self) -> bool {
        // clippy wants use of matches! macro
        matches!(self, RedfishError::HTTPErrorCode {
                url: _,
                status_code,
                response_body: _,
            } if *status_code == StatusCode::NOT_FOUND)
    }
}
