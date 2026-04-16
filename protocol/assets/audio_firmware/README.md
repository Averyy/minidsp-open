This directory is the checked-in XML source of truth for reset/default preset
generation in `minidsp-protocol`.

Normal desktop and ESP32 builds must depend only on the files in this tree, not
on the private reverse-engineered `app/Audio Firmware/` extraction tree.

Current contents:

- one checked-in `DSP_default_parameters.xml` per supported modern SHARC family
- enough XML to generate the embedded default preset reset assets

Runtime note:

- the desktop binary and ESP32 firmware do not load these XML files directly at
  runtime
- `protocol/build.rs` parses them at build time and emits compact embedded
  preset blobs into `OUT_DIR`

Upstream maintenance note:

- when the private extracted corpus is refreshed, copy the updated
  `DSP_default_parameters.xml` files into this tree and rebuild/regenerate
  normally
