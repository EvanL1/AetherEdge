# Energy automation commissioning example

This directory contains only disabled site-configuration examples. The
versioned rule source of truth is
[`../../../rules/battery_soc_management.json`](../../../rules/battery_soc_management.json),
declared exactly once by [`../../../rules/index.yaml`](../../../rules/index.yaml).

Activating the Energy Pack does not copy, import, schedule, or enable that rule.
Commissioning must bind the referenced logical instances and points, review its
device commands, import it through the governed application interface, and
explicitly enable it.
