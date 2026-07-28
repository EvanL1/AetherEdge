# ADR-0009: Introduce optional, industry-neutral data processing

## Status

Superseded by [ADR-0028](0028-move-derived-data-processing-downstream.md) on
2026-07-28.

## Historical decision

This ADR introduced an optional generic derived-data processor boundary with
bounded inputs, non-authoritative outputs, no SHM writer, and no direct device
control. Its only concrete product consumer became energy forecasting.

The implementation and contracts have been removed from AetherEdge. Derived
data, forecasting, covariates, model artifacts, and processor transport now
belong to downstream applications such as AetherEMS. See ADR-0028 for the
rationale, removed surfaces, and compatibility decision.
