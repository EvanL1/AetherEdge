# IO configuration

The default distribution intentionally contains no commissioned channels. A
fresh installation therefore does not open a fieldbus, serial device, CAN
interface, broker connection, or polling endpoint.

Copy an appropriate domain-pack example into the active configuration only as
part of commissioning. Keep every channel disabled while editing addresses and
point mappings, validate the configuration, and enable it explicitly after the
operator approves the plan.
