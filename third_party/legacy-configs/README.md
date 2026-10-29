# Legacy device XML provenance

The `*/config.xml` files are manufacturer-derived SigmaStudio configuration
exports obtained from [minidsp-rs devtools/src/codegen](https://github.com/mrene/minidsp-rs/tree/dev/devtools/src/codegen).
They supply register addresses, filters, labels, and device metadata for
interoperability with legacy MiniDSP hardware. Our extraction code is original;
the source XML is vendor data.

Credit to Mathieu Rene and the minidsp-rs contributors. minidsp-rs is licensed
under Apache-2.0; see the [included license](../licenses/minidsp-rs-Apache-2.0.txt)
and the root [NOTICE](../../NOTICE) for attribution and scope.

MiniDSP and other original rights holders retain their rights in these exports.
The XML is excluded from minidsp-open's MIT license. Upstream availability and
this notice do not establish a separate MiniDSP redistribution grant. Retain
these notices with the XML and generated vendor-data tables.
