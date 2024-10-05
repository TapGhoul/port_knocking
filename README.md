# Port Knocking

A little port knocking toy I threw together in my spare time.

Doesn't do any firewall stuff, and could probably be built better, with loads of potential points of panic, but it's a
fun little toy. This is only tested to work on linux, and uses a linux ethernet packet socket to do so.

To run without root, run `sudo setcap CAP_NET_RAW=ep <binary>` to apply the required capabilities.

This is not a security product. Do not use this for anything mission-critical.
