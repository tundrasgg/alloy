# ADR 0062: The Windows client may ship the signed Wintun DLL unmodified

- Status: accepted
- Subject: engineering
- Specs: arfc-0002, arfc-0017

## Context

A Windows client node needs a layer 3 virtual interface for `al0` (ARFC 0002 section 3.4), and Windows has none built in. Wintun provides one as a signed DLL that its project says may be shipped with other software, and calls that the only supported way to distribute it (https://www.wintun.net/). The prebuilt DLL is not under a permissive license: its own license allows redistribution only together with software that uses it through its published API, and forbids modifying it (https://git.zx2c4.com/wintun/plain/prebuilt-binaries-license.txt). ADR 0035 admits only permissive terms, so shipping it needs an exception. The pilot granted it (D-0036).

## Decision

- The Windows archives of a release (ARFC 0017 clause 3.13) may contain `bin/wintun.dll`. No other artifact may contain it or any other part of Wintun.
- The DLL is the one from the Wintun project's release zip for the archive's architecture, byte for byte, with its signature intact. The release workflow pins the zip's version and SHA-256 in the repository, refuses a download that does not match, and checks that the DLL's Authenticode signature verifies before it packages it.
- The Wintun license text from the same zip ships unchanged as `licenses/wintun.txt` next to the DLL, and no notice in the zip is removed.
- Alloy code loads the DLL at run time and calls only the functions its published header declares. Alloy does not modify, link statically, or vendor Wintun source, and the DLL is never committed to the repository.
- A Rust crate that binds the Wintun API is a dependency like any other and passes `agent/eng/deps.md`. This exception covers the DLL only.

## Consequences

- Windows client nodes get `al0` with no driver of Alloy's own to write and sign, and rung 6 of M11 can route Windows to VPC addresses.
- The Windows archives are not entirely under permissive terms. Their license listing says so, and a redistributor of them must keep the Wintun license.
- If Wintun's license changes, or the project stops publishing signed builds, this decision is reopened before the next release that would ship the DLL.
- Creating Alloy's first Wintun adapter installs the Wintun driver when it is missing. Other products load the same driver, and whether one still needs it cannot be seen while its tunnel is down, so Alloy never removes the driver: an uninstall removes only Alloy's own adapters and files (ARFC 0017 clauses 3.16 and 3.17).

## Alternatives rejected

- Write and sign an Alloy virtual interface driver: a kernel driver and its Windows signing are far more work and risk than rung 6 justifies.
- Another third-party virtual interface driver: it needs its own license exception and review, for no gain named over Wintun.
- A Windows client with no virtual interface, reaching VPCs by a local proxy: only programs that speak the proxy could reach VPC addresses, which fails rung 6's SSH as an ordinary program.
- Admit Wintun's license in `deny.toml` or the global license list: the DLL is not a crate, and a general entry would admit the terms for anything.
