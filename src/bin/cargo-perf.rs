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

use cargo_metadata::Message;
use clap::Parser;
use inferno::collapse::Collapse;
use std::io;
use std::io::{BufReader, BufWriter};
use std::process::{Command, Stdio};

use perf_tools::pprof;

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// run perf and generate pprof
    Perf(Args),
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// command to run
    #[clap(short, long)]
    bin: Option<String>,

    /// output file name
    #[clap(short, long)]
    output: Option<String>,

    /// sampling frequency
    #[clap(long)]
    frequency: Option<u32>,

    /// generate flamegraph instead of pprof
    #[clap(long)]
    flamegraph: bool,
}

fn build_binary(args: &Args) -> std::io::Result<Vec<cargo_metadata::Artifact>> {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "build",
        "--release",
        "--message-format=json-render-diagnostics",
    ]);

    if let Some(bin) = &args.bin {
        cmd.arg("--bin");
        cmd.arg(bin);
    }

    let mut command = cmd
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to run `cargo build`");

    let reader = std::io::BufReader::new(command.stdout.take().unwrap());
    Ok(cargo_metadata::Message::parse_stream(reader)
        .filter_map(|m| {
            if let Ok(Message::CompilerArtifact(m)) = m {
                if m.executable.is_some() {
                    Some(m)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect::<Vec<cargo_metadata::Artifact>>())
}

fn find_binary(args: &Args, artifact: &[cargo_metadata::Artifact]) -> std::io::Result<String> {
    if artifact.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "can't find any binary",
        ));
    }

    if let Some(name) = args.bin.as_ref() {
        for a in artifact {
            if a.executable.as_ref().unwrap().ends_with(name) {
                return Ok(a.executable.as_ref().unwrap().to_string());
            }
        }
        Err(io::Error::new(
            io::ErrorKind::Other,
            "can't find binary name to be specified",
        ))
    } else {
        if artifact.len() == 1 {
            return Ok(artifact[0].executable.as_ref().unwrap().to_string());
        }
        Err(io::Error::new(
            io::ErrorKind::Other,
            "found multiple binaries; specify one with `--bin` option",
        ))
    }
}

const PERF_DATA_FILE: &str = "perf.data";
const DEFAULT_PPROF_OUTPUT: &str = "cpu.pprof";
const DEFAULT_FLAMEGRAPH_OUTPUT: &str = "flamegraph.svg";
const DEFAULT_RECORD_FREQ: u32 = 99;

fn main() {
    let Commands::Perf(args) = Cli::parse().command;

    let artifact = build_binary(&args).unwrap();
    let binary_path = find_binary(&args, &artifact).unwrap();

    let mut cmd = Command::new("perf");
    cmd.args([
        "record",
        "--call-graph",
        "dwarf",
        "-g",
        "-F",
        &format!("{}", args.frequency.unwrap_or(DEFAULT_RECORD_FREQ)),
        "-o",
        PERF_DATA_FILE,
    ]);
    cmd.arg(binary_path);
    cmd.spawn()
        .unwrap_or_else(|e| panic!("failed to run {:?}", e))
        .wait_with_output()
        .map(|output| {
            if output.status.success() {
                println!("{}", String::from_utf8(output.stdout).unwrap());
            } else {
                panic!("{}", String::from_utf8(output.stderr).unwrap());
            }
        })
        .expect("failed to wait for `perf record`");

    let script_output = Command::new("perf")
        .arg("script")
        .arg("--header")
        .output()
        .expect("failed to execute perf");
    if !script_output.status.success() {
        panic!("{}", String::from_utf8(script_output.stderr).unwrap());
    }

    let output = args.output.unwrap_or_else(|| {
        if args.flamegraph {
            DEFAULT_FLAMEGRAPH_OUTPUT.to_string()
        } else {
            DEFAULT_PPROF_OUTPUT.to_string()
        }
    });
    let writer = std::fs::File::create(output).expect("failed to create output file");
    let perf_reader = BufReader::new(&*script_output.stdout);
    if args.flamegraph {
        let mut collapsed = vec![];
        inferno::collapse::perf::Folder::default()
            .collapse(perf_reader, BufWriter::new(&mut collapsed))
            .unwrap();

        inferno::flamegraph::from_reader(
            &mut inferno::flamegraph::Options::default(),
            BufReader::new(&*collapsed),
            &writer,
        )
        .unwrap();
    } else {
        let mut encoder = libflate::gzip::Encoder::new(writer).unwrap();
        pprof::PprofConverterBuilder::default()
            .build()
            .from_reader(perf_reader, &mut encoder)
            .unwrap();
        encoder.finish().into_result().unwrap();
    }
}
