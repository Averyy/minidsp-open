This directory is the checked-in XML source of truth for reset/default preset
generation in `minidsp-protocol`.

## Provenance and rights

The `*/DSP_default_parameters.xml` files are MiniDSP Device Console app
resources, extracted from
`Contents/Resources/device_support_package/DSP_Structures/`. They are included
for interoperability: interpreting device registers and generating default
preset/reset payloads. MiniDSP retains its rights in these files. They and their
generated vendor-data descendants are excluded from this repository's MIT
license; this notice does not grant a separate redistribution license.

See the root [NOTICE](../../../NOTICE) for vendor-data scope and attribution to
minidsp-rs (protocol knowledge and the separate legacy SigmaStudio export
corpus, Apache-2.0). Its [license](../../../third_party/licenses/minidsp-rs-Apache-2.0.txt)
does not relicense MiniDSP's underlying resources. Preserve these provenance
notices when refreshing or distributing the assets and generated payloads.

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
