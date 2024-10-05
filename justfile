#!/usr/bin/env just --justfile

run:
    cargo build
    sudo setcap CAP_NET_RAW=ep target/debug/port_knocking
    target/debug/port_knocking

bacon-clippy-aggressive:
    bacon -j clippy -- -- -W clippy::all
