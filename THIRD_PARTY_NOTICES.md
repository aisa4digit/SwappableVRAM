# Upstream attribution

`drivers/kmdf/control.c` follows the control-device sideband pattern from Microsoft's Windows Driver Samples / Toaster (`general/toaster/toastDrv/kmdf/filter/sideband/filter.c`). Copyright (c) Microsoft Corporation. That file's adaptation and source distribution retain the Microsoft Public License (MS-PL), whose full text is in `third_party/driver-samples/LICENSE-MS-PL.txt`. Reference: https://github.com/microsoft/Windows-driver-samples/blob/main/general/toaster/toastDrv/kmdf/filter/sideband/filter.c . The earlier raw-PDO experiment was replaced. This component notice does not assign a license to unrelated original project files.

The optional Rust `wddm` feature uses Microsoft's `windows` / windows-rs 0.62.2 bindings and their locked dependencies. Original license texts, including the Unicode data license, are retained under `third_party/rust-licenses`; `scripts/Export-RustLicenses.ps1` refreshes them from Cargo package metadata. These bindings are not required by the default minimal WinPE storage CLI.

The CUDA Driver API block-store approach is based on **nbd-vram** by Sean Lobjoit (c0dejedi): https://github.com/c0dejedi/nbd-vram . Inspected commit: `e2f1144f93c6f508b01a2ff435509e27853c0632`.

The Windows implementation is a new Rust/KMDF/WPF implementation. The Linux NBD daemon, kernel integration and Linux deployment scripts are not shipped as a Windows backend.

Upstream license retained:

```text
MIT License

Copyright (c) 2026 Sean Lobjoit (c0dejedi)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

**RamShared** was inspected for architecture and Windows implementation status: https://github.com/emersonbusson/ramshared , commit `1bcc98068218f48aa357103b21e7ee692df91939`. Its driver, services and source are not incorporated in this implementation.

The optional self-contained desktop publish includes Microsoft .NET runtime files and their bundled notices. Microsoft WDK/SDK development packages remain in the ignored project-local toolchain cache and are not included with the desktop app.
