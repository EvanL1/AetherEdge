# Energy API commissioning assets

`calculated_points.sql` preserves the legacy AetherEMS energy-dashboard preset
as a distribution migration and commissioning asset. It is data owned by the
Energy Pack, not a Kernel default.

Installing or activating this Pack does not execute the SQL. A distribution
installer or operator may apply it explicitly during an offline, backed-up
commissioning migration, then must replace the empty formulas with bindings
verified for that site. A fresh Aether Kernel database and the homepage reset
operation both remain at the safe empty state with zero calculated points.
