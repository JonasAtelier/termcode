# Serial (xcomm)

termcode's Serial view, as an extension on the [xcomm](https://crates.io/crates/xcomm) crate.

- **Ports**: every serial port with its state (green free, yellow held by an xcomm
  session, red held by something else), its adapter and your note. Rest the pointer on
  one for everything about it.
- **Monitor** a port to watch it in a tab (read only); **Attach** to type into it.
  A port another xcomm holds is shared with it, after you agree.
- **Settings** on the right: baud, data bits, parity, stop bits, flow, DTR/RTS, break,
  capture to a file, what agents may do.
- The wheel scrolls back through a channel's output; drag to select, Ctrl+C to copy.
- F1: **Open a Port…**, **Close All Channels**.

Agents and scripts reach the ports through termcode's API (`serial.ports`, `serial.monitor`,
`serial.send`, `serial.wait`, …, and `serial.output` events), or with `xcomm ctl`.
