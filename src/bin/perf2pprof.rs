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

use clap::Parser;
use std::process::Command;

use perf_tools::pprof;

/// convert perf to pprof format
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// input file name
    #[clap(short, long, default_value = "perf.data")]
    input: String,

    /// output file name
    #[clap(short, long, default_value = "cpu.pprof")]
    output: String,
}

fn main() {
    let args = Args::parse();

    let output = Command::new("perf")
        .arg("script")
        .arg("--header")
        .arg("-i")
        .arg(&args.input)
        .output()
        .expect("failed to execute perf");

    if !output.status.success() {
        panic!("{}", String::from_utf8(output.stderr).unwrap());
    }

    let mut encoder =
        libflate::gzip::Encoder::new(std::fs::File::create(args.output).unwrap()).unwrap();
    pprof::PprofConverterBuilder::default()
        .build()
        .from_reader(
            std::io::BufReader::with_capacity(4096, &*output.stdout),
            &mut encoder,
        )
        .unwrap();

    encoder
        .finish()
        .into_result()
        .expect("gzip encoding failed");
}
