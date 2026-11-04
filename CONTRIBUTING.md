# Contributing to SwappableVRAM

Issues, forks and pull requests are welcome. This is an experimental Windows
memory project: system-wide RAM offload is not working in the current build,
and driver signing/loading and end-to-end paging validation remain incomplete.
Please read the [README](README.md) before proposing changes or running tests.

## Proposing a change

1. Open an issue for a bug, feature request or substantial design change. Explain
   the problem and the behavior you expect.
2. Fork the repository and create a branch for a focused change.
3. Include relevant documentation and validation results in your pull request.
   Distinguish unit tests, real GPU tests, kernel paging tests and game tests.
4. Submit the pull request against `main`. The repository owner,
   [@aisa4digit](https://github.com/aisa4digit), reviews and approves changes
   before they are merged into the official repository. Opening a pull request
   does not grant write access.

You can keep and publish changes in your own fork without asking for approval,
subject to the applicable licenses. Preserve the `aisa4digit` copyright and MIT
permission notice in copies or substantial portions of the original project.
Retain third-party notices and component-specific licenses as well.

## Development checks

From a Windows development environment, run the checks relevant to your change:

```powershell
powershell -NoProfile -File scripts/Test-Source.ps1 -Wddm
dotnet build SwappableVRAM.sln -c Release
```

For broker-control changes, publish the desktop package and test the real local
controller/helper connection:

```powershell
powershell -NoProfile -File scripts/Build-Desktop.ps1
dotnet run --project src/SwappableVRAM.Diagnostics -c Release -- controller-test artifacts/desktop/SwappableVRAM.Broker.exe
```

Do not present a successful build, CUDA copy or driver attachment as proof of
application-memory offload or better FPS. Include the hardware, workload,
measurement method and limits of any performance result. Kernel changes need
appropriate paging, integrity, cancellation and failure-path validation; see
[the driver status notes](docs/DRIVER-STATUS.md).

Keep generated binaries, downloaded toolchains, VM images, signing keys, tokens
and local test outputs out of commits. License contributions to original project
code under the [MIT License](LICENSE); components with an existing separate
license retain that license. Do not relicense third-party code through a pull
request.
