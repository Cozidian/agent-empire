# Upstream license supplements

`dasp_sample` 0.11.0 omits its workspace-level license files from the crates.io archive.
The copies in `dasp_sample-0.11.0/` come from its `.cargo_vcs_info.json` source revision:

- https://github.com/RustAudio/dasp/blob/97c3bb9b2363c0b46ac1633858bf1054fd02a980/LICENSE-MIT
- https://github.com/RustAudio/dasp/blob/97c3bb9b2363c0b46ac1633858bf1054fd02a980/LICENSE-APACHE

The notice generator uses these only for this exact package/version. Other dependencies must
supply license files in their published source; missing texts fail the generation step.
