// SPDX-License-Identifier: GPL-3.0-only
use std::process::Command;

fn main() {
    if let Some(output) = Command::new("git")
        .args(&["rev-parse", "HEAD"])
        .output()
        .ok()
    {
        let git_hash = String::from_utf8(output.stdout).unwrap();
        println!("cargo:rustc-env=GIT_HASH={}", git_hash);
    }
}
