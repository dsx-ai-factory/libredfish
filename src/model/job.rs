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
use crate::model::{
    task::{Task, TaskState},
    ODataLinks,
};
use serde::{Deserialize, Serialize};

// A "Job" is very similar to a "Task", but there are a few key differences.
// We won't export this struct for now, instead cramming the info into a "Task" struct for ease of use.

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct Job {
    #[serde(flatten)]
    pub odata: ODataLinks,
    pub id: Option<String>,
    pub name: Option<String>,
    pub percent_complete: Option<u32>,
    pub job_state: Option<TaskState>,
}

impl Job {
    pub fn as_task(&self) -> Task {
        Task {
            odata: self.odata.clone(),
            id: self.id.clone().unwrap_or("".to_string()),
            messages: vec![],
            name: self.name.clone(),
            task_state: self.job_state.clone(),
            task_status: None,
            task_monitor: None,
            percent_complete: self.percent_complete,
            start_time: None,
            end_time: None,
            payload: None,
            hide_payload: None,
        }
    }
}
