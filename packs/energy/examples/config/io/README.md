# Energy Pack IO commissioning example

This directory contains only disabled channel configuration examples. Formal,
versioned Pack mappings live under [`../../../mappings`](../../../mappings) and
are listed exactly once by its `index.yaml`.

The Pack intentionally ships no claimed device-register map for the placeholder
PCS/BMS endpoints: register addresses, word order, writable functions, limits,
and device revision must be verified from the commissioned device contract.
Activating the Pack never enables these channels or creates protocol mappings.
