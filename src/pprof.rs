// Copyright (C) 2022 The Perf-tools Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
// implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use chrono::{offset::LocalResult, DateTime, Local, NaiveDateTime, TimeZone};
use lazy_static::lazy_static;
use prost::Message;
use regex::Regex;
use std::collections::HashMap;
use std::io;
use std::io::Write;
use std::time::Duration;

pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/perftools.profiles.rs"));
}

#[derive(PartialEq, Hash, std::cmp::Eq)]
struct Stack {
    pc: u64,
    func: String,
    module: String,
}

#[derive(PartialEq, Hash, std::cmp::Eq)]
struct Sample {
    stacks: Vec<Stack>,
}

struct PerfReader {
    sample: HashMap<Sample, u64>,
    captured_time: DateTime<Local>,
    duration: Duration,
    freq: u64,
}

#[derive(Default)]
pub struct PprofConverterBuilder {}

impl PprofConverterBuilder {
    pub fn build(&mut self) -> PprofConverter {
        PprofConverter::new()
    }
}

impl PerfReader {
    fn new<R>(mut reader: R) -> io::Result<Self>
    where
        R: io::BufRead,
    {
        let mut buf = Vec::new();
        let mut is_event_line = true;
        let mut sample = HashMap::default();
        let mut header = Vec::new();
        let mut stack = Vec::new();
        let mut start_usec = 0;
        let mut end_usec = 0;

        lazy_static! {
            static ref RE: Regex = Regex::new(r"\S+\s+\d+\s+(\d+)\.(\d+)").unwrap();
        }

        loop {
            buf.clear();
            if let Ok(n) = reader.read_until(b'\n', &mut buf) {
                if n == 0 {
                    break;
                }
                let line = String::from_utf8_lossy(&buf);
                if line.starts_with('#') {
                    header.push(line.trim().to_string());
                    continue;
                }
                let line = line.trim();
                if line.is_empty() {
                    // return one stack
                    is_event_line = true;
                    if !stack.is_empty() {
                        let count = sample
                            .entry(Sample {
                                stacks: stack.split_off(0),
                            })
                            .or_insert(0);
                        *count += 1;
                    }
                    continue;
                }
                if is_event_line {
                    // event line
                    if let Some(caps) = RE.captures(line) {
                        let sec: u64 = caps.get(1).unwrap().as_str().parse().unwrap();
                        let usec: u64 = caps.get(2).unwrap().as_str().parse().unwrap();
                        if sample.is_empty() {
                            start_usec = sec * 1_000_000 + usec;
                        } else {
                            end_usec = sec * 1_000_000 + usec;
                        }
                    }

                    is_event_line = false;
                    continue;
                } else {
                    // stack line
                    let line = line.splitn(2, ' ').collect::<Vec<&str>>();
                    if let Ok(pc) = u64::from_str_radix(line[0], 16) {
                        let line = line[1].rsplitn(2, ' ').collect::<Vec<&str>>();
                        stack.push(Stack {
                            pc,
                            func: line[1].to_string(),
                            module: line[0].to_string(),
                        });
                    }
                }
            } else {
                break;
            }
        }

        if end_usec == 0 {
            return Err(io::Error::new(io::ErrorKind::Other, "can't find duration"));
        }

        let (captured_time, freq) = PerfReader::verify_header(&header)?;

        Ok(PerfReader {
            sample,
            captured_time,
            duration: Duration::from_micros(end_usec - start_usec),
            freq,
        })
    }

    fn verify_header(header: &[String]) -> io::Result<(DateTime<Local>, u64)> {
        let mut dt = None;
        let mut freq = 0;

        for h in header {
            // sample_freq } = 997
            let re = Regex::new(r"sample_freq\s+}\s+=\s+(\d+)").unwrap();
            // captured on    : Thu Mar 10 10:45:19 2022
            if h.contains("captured on") {
                let line = h.splitn(2, ':').collect::<Vec<&str>>();
                if line.len() == 2 {
                    if let Ok(time) = NaiveDateTime::parse_from_str(line[1].trim(), "%c") {
                        dt = if let LocalResult::Single(t) = Local.from_local_datetime(&time) {
                            Some(t)
                        } else {
                            None
                        };
                    }
                }
            } else if let Some(caps) = re.captures(h) {
                if let Some(v) = caps.get(1) {
                    freq = v
                        .as_str()
                        .parse()
                        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{}", e)))?;
                }
            }
        }
        let captured_time = dt.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "captured time isn't found in the header",
            )
        })?;
        if freq == 0 {}

        Ok((captured_time, freq))
    }
}

pub struct PprofConverter {
    str_map: HashMap<String, u64>,

    location: LocationId,
    function: FunctionId,
}

struct FunctionId {
    next_id: u64,
    map: HashMap<String, (u64, u64)>, // name, (id, str_id)
}

struct LocationId {
    next_id: u64,
    map: HashMap<u64, (u64, u64)>, // address, (id, funciton_id)
}

impl PprofConverter {
    fn new() -> Self {
        let mut str_map: HashMap<String, u64> = HashMap::default();
        for (i, s) in vec!["", "samples", "count", "cpu", "nanoseconds"]
            .iter()
            .enumerate()
        {
            str_map.insert(s.to_string(), i as u64);
        }

        PprofConverter {
            str_map,
            location: LocationId {
                next_id: 0,
                map: HashMap::default(),
            },
            function: FunctionId {
                next_id: 0,
                map: HashMap::default(),
            },
        }
    }

    fn location_id(&mut self, addr: u64, name: &str) -> u64 {
        let loc_id = self.location.map.entry(addr).or_insert_with(|| {
            self.location.next_id += 1;
            let func_id = self
                .function
                .map
                .entry(name.to_string())
                .or_insert_with(|| {
                    let s = self.str_map.len() as u64;
                    let str_id = self.str_map.entry(name.to_string()).or_insert(s);
                    self.function.next_id += 1;
                    (self.function.next_id, *str_id)
                });
            (self.location.next_id, func_id.0)
        });
        loc_id.0
    }

    fn finish<R, W>(&mut self, reader: R, writer: W) -> io::Result<()>
    where
        R: io::BufRead,
        W: io::Write,
    {
        let perf = PerfReader::new(reader)?;
        let sample: Vec<pb::Sample> = perf
            .sample
            .iter()
            .map(|(s, count)| pb::Sample {
                location_id: s
                    .stacks
                    .iter()
                    .map(|s| self.location_id(s.pc, &s.func))
                    .collect(),
                value: vec![
                    *count as i64,
                    *count as i64 * 1_000_000_000 / perf.freq as i64,
                ],
                label: Vec::new(),
            })
            .collect();

        let mut function: Vec<pb::Function> = self
            .function
            .map
            .iter()
            .map(|(_, v)| pb::Function {
                id: v.0,
                name: v.1 as i64,
                ..Default::default()
            })
            .collect();
        function.sort_by(|a, b| a.id.cmp(&b.id));

        let mut string_table: Vec<(String, u64)> =
            self.str_map.iter().map(|(k, v)| (k.clone(), *v)).collect();
        string_table.sort_by(|a, b| a.1.cmp(&b.1));

        let mut location: Vec<pb::Location> = self
            .location
            .map
            .iter()
            .map(|(k, v)| pb::Location {
                id: v.0,
                address: *k,
                line: vec![pb::Line {
                    function_id: v.1,
                    line: 0,
                }],
                ..Default::default()
            })
            .collect();
        location.sort_by(|a, b| a.id.cmp(&b.id));

        let mut content = Vec::new();
        pb::Profile {
            sample_type: vec![
                pb::ValueType { r#type: 1, unit: 2 },
                pb::ValueType { r#type: 3, unit: 4 },
            ],
            sample,
            location,
            function,
            time_nanos: perf.captured_time.timestamp_nanos(),
            duration_nanos: perf.duration.as_nanos() as i64,
            string_table: string_table.into_iter().map(|(k, _)| k).collect(),
            period: 1_000_000_000 / perf.freq as i64,
            period_type: Some(pb::ValueType { r#type: 3, unit: 4 }),
            ..pb::Profile::default()
        }
        .encode(&mut content)?;
        let mut encoder = libflate::gzip::Encoder::new(writer)?;
        encoder.write_all(&content)?;
        encoder.finish().into_result().map(|_| ())
    }

    pub fn from_reader<R, W>(&mut self, reader: R, writer: W) -> io::Result<()>
    where
        R: io::BufRead,
        W: io::Write,
    {
        self.finish(reader, writer)
    }
}
