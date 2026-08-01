---
title: Home Assistant integration extraction
description: Migrate installations away from the retired in-kernel Home Assistant bridge
---

# Home Assistant integration extraction

The experimental Home Assistant bridge is removed from the minimal
AetherEdge kernel. Standard IO binaries no longer expose the
`home-assistant`, `home-assistant-cloudlink`, or
`home-assistant-integration-control` features and ignore none of their former
settings: installer feature selection fails and no bridge process starts.

Before upgrading an installation that used an experimental custom build:

1. disable Home Assistant delegated-device commands and wait for in-flight
   receipts to reach a terminal state;
2. archive the bridge topology-generation store, control ledger, audit log, and
   CloudLink spools according to site retention policy;
3. revoke the dedicated Home Assistant token and remove the former
   `AETHER_HOME_ASSISTANT_*` secrets and settings;
4. replace the integration with a downstream Rust composition that consumes
   Aether SDK and port contracts, or commission the physical devices through a
   supported IO protocol; and
5. verify the live runtime capability catalog before re-enabling any remote
   command source.

A downstream integration must remain deny-by-default for writes, use the
shared application command boundary, keep provider credentials outside Aether
state and prompts, and never write SHM or kernel storage directly. AetherEdge
currently ships no generic dynamic plugin host and does not load downstream
scripts, shared libraries, or child processes.

This migration page does not advertise Home Assistant as a capability of the
standard kernel.
